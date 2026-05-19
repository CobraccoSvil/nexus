#!/usr/bin/env bash
cd /home/administrator/ideai
python3 -m py_compile brain/redaction/__init__.py brain/redaction/client.py brain/tests/test_redaction.py && echo "syntax OK"
python3 -c "
import brain.redaction as r
print('exports:', sorted([n for n in dir(r) if not n.startswith('_')]))
print('PresidioClient instance:', r.PresidioClient())
"
