"""WebSocket terminale: /ws/terminal/{session_id}.

Apre una shell PTY scoped alla sessione di progetto emessa da MCP Core. La
verifica del token firmato e la preparazione del comando shell (guard bash che
impedisce l'uscita dalla root del progetto) vivono in
`brain.grpc_server.runtime`.
"""
from __future__ import annotations

import asyncio
import json as json_mod
import logging
import os
import select
import struct

from fastapi import APIRouter, WebSocket, WebSocketDisconnect

from brain.grpc_server import runtime

try:
    import fcntl
    import pty
    import termios

    POSIX_PTY = True
except ImportError:
    fcntl = None
    pty = None
    termios = None
    POSIX_PTY = False

logger = logging.getLogger(__name__)

router = APIRouter()


@router.websocket("/ws/terminal/{session_id}")
async def terminal_ws(websocket: WebSocket, session_id: str):
    """WebSocket terminal scoped to a project session emitted by MCP Core."""
    await websocket.accept()
    token = websocket.query_params.get("token")

    payload = runtime._verify_terminal_token(token)
    if payload is None or payload.get("sid") != session_id:
        await websocket.send_text("[Terminal session non valida]")
        await websocket.close(code=4403)
        return

    cwd = str(payload["cwd"])
    env = os.environ.copy()
    env["TERM"] = "xterm-256color"
    shell_command, rc_path = runtime._prepare_shell_command(payload)

    if POSIX_PTY:
        import subprocess

        master_fd, slave_fd = pty.openpty()
        proc = subprocess.Popen(
            shell_command,
            stdin=slave_fd,
            stdout=slave_fd,
            stderr=slave_fd,
            cwd=cwd,
            env=env,
            preexec_fn=os.setsid,
        )
        os.close(slave_fd)

        # ── Server-side output buffer ──
        import re as _re
        _output_buf: list[bytes] = []
        _output_buf_len = 0
        _max_buf = 16384  # 16KB ring buffer
        _project_id = payload.get("pid", "")
        _db_url = os.environ.get("DATABASE_URL")

        def _strip_ansi(s: str) -> str:
            s = _re.sub(r"\x1B\[[0-9;]*[A-Za-z]", "", s)
            s = _re.sub(r"\x1B\][^\x07]*\x07", "", s)
            s = _re.sub(r"\x1B\([A-Z]", "", s)
            s = s.replace("\r", "")
            return s.strip()

        def _flush_output_to_db(exit_code_val=None):
            """Scrive il buffer output nel DB per l'ultimo comando della sessione."""
            if not _db_url:
                return
            try:
                raw = b"".join(_output_buf).decode("utf-8", errors="replace")
                clean = _strip_ansi(raw)[-8000:]
                from brain.utils.db_pool import connect as _db_connect
                with _db_connect() as conn, conn.cursor() as cur:
                    cur.execute(
                        "UPDATE terminal_commands "
                        "SET full_output = %s, exit_code = %s, finished_at = NOW() "
                        "WHERE id = ("
                        "  SELECT id FROM terminal_commands "
                        "  WHERE session_id = %s AND full_output IS NULL "
                        "  ORDER BY created_at DESC LIMIT 1"
                        ")",
                        (clean, exit_code_val, session_id),
                    )
                logger.debug("_flush_output_to_db: wrote %d chars, exit=%s", len(clean), exit_code_val)
            except Exception as e:
                logger.debug("_flush_output_to_db error: %s", e)

        async def _periodic_flush():
            """Debounce server-side: dopo 5s di output stabile, flush al DB."""
            last_len = 0
            stable_count = 0
            try:
                while proc.poll() is None:
                    await asyncio.sleep(1)
                    cur_len = _output_buf_len
                    if cur_len == last_len:
                        stable_count += 1
                        if stable_count >= 5 and cur_len > 0:
                            _flush_output_to_db(exit_code_val=None)
                            stable_count = 0
                    else:
                        last_len = cur_len
                        stable_count = 0
            except asyncio.CancelledError:
                pass

        async def read_pty():
            nonlocal _output_buf_len
            loop = asyncio.get_event_loop()
            try:
                while proc.poll() is None:
                    ready = await loop.run_in_executor(
                        None, lambda: select.select([master_fd], [], [], 0.1)[0]
                    )
                    if ready:
                        try:
                            output = os.read(master_fd, 4096)
                            if not output:
                                break
                            await websocket.send_bytes(output)
                            # Buffer server-side
                            _output_buf.append(output)
                            _output_buf_len += len(output)
                            while _output_buf_len > _max_buf and _output_buf:
                                removed = _output_buf.pop(0)
                                _output_buf_len -= len(removed)
                        except OSError:
                            break
            except Exception as e:
                logger.debug("read_pty ended: %s", e)
            # Processo terminato: flush finale con exit code
            exit_code = proc.poll()
            _flush_output_to_db(exit_code_val=exit_code)
            if exit_code is not None:
                try:
                    await websocket.send_text(json_mod.dumps({
                        "type": "process_exit",
                        "exitCode": exit_code,
                    }))
                except Exception:
                    pass

        flush_task = asyncio.create_task(_periodic_flush())
        reader_task = asyncio.create_task(read_pty())

        try:
            while True:
                data = await websocket.receive()
                if "bytes" in data:
                    os.write(master_fd, data["bytes"])
                elif "text" in data:
                    text = data["text"]
                    if text.startswith("{"):
                        try:
                            msg = json_mod.loads(text)
                            if msg.get("type") == "resize":
                                winsize = struct.pack("HHHH", msg["rows"], msg["cols"], 0, 0)
                                fcntl.ioctl(master_fd, termios.TIOCSWINSZ, winsize)
                                continue
                        except (json_mod.JSONDecodeError, KeyError):
                            pass
                    os.write(master_fd, text.encode())
        except WebSocketDisconnect:
            pass
        except Exception as e:
            logger.debug("terminal_ws ended: %s", e)
        finally:
            flush_task.cancel()
            reader_task.cancel()
            try:
                os.close(master_fd)
            except OSError:
                pass
            try:
                proc.terminate()
                proc.wait(timeout=2)
            except Exception:
                proc.kill()
            if rc_path:
                try:
                    os.unlink(rc_path)
                except OSError:
                    pass
        return

    proc = await asyncio.create_subprocess_exec(
        *shell_command,
        cwd=cwd,
        env=env,
        stdin=asyncio.subprocess.PIPE,
        stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.STDOUT,
    )

    async def read_stream():
        try:
            while True:
                chunk = await proc.stdout.read(4096)
                if not chunk:
                    break
                await websocket.send_bytes(chunk)
        except Exception as e:
            logger.debug("read_stream ended: %s", e)

    reader_task = asyncio.create_task(read_stream())

    try:
        while True:
            data = await websocket.receive()
            if "bytes" in data and proc.stdin:
                proc.stdin.write(data["bytes"])
                await proc.stdin.drain()
            elif "text" in data and proc.stdin:
                text = data["text"]
                if text.startswith("{"):
                    try:
                        msg = json_mod.loads(text)
                        if msg.get("type") == "resize":
                            continue
                    except json_mod.JSONDecodeError:
                        pass
                proc.stdin.write(text.encode())
                await proc.stdin.drain()
    except WebSocketDisconnect:
        pass
    except Exception as e:
        logger.debug("terminal_ws ended: %s", e)
    finally:
        reader_task.cancel()
        try:
            if proc.stdin:
                proc.stdin.close()
        except Exception:
            pass
        if proc.returncode is None:
            proc.terminate()
            await proc.wait()
        if rc_path:
            try:
                os.unlink(rc_path)
            except OSError:
                pass
