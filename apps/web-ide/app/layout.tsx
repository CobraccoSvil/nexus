import type { ReactNode } from "react";
import localFont from "next/font/local";
import "./globals.css";
import "@xterm/xterm/css/xterm.css";
import { ThemeBody } from "./theme-body";

// PUNTO UNICO tipografico (regola L): JetBrains Mono self-hosted (file variable
// .woff2 nel repo, licenza in app/fonts/OFL.txt) — nessuna richiesta runtime a
// font esterni. Espone --font-mono, applicata a <html> e usata dal body e da
// tutti gli inline style. Il font e il fallback si cambiano SOLO qui.
const jetBrainsMono = localFont({
  src: "./fonts/jetbrains-mono-latin-wght-normal.woff2",
  weight: "100 800",
  style: "normal",
  display: "swap",
  variable: "--font-mono",
  fallback: ["Fira Code", "monospace"],
});

export const metadata = {
  title: "Nexus",
  description: "Web IDE and dashboard for Nexus",
};

const themeScript = `(function(){
  try {
    var t = localStorage.getItem("nexus-theme") || "dark";
    var resolved = t;
    if (t === "auto") {
      resolved = window.matchMedia("(prefers-color-scheme: light)").matches ? "light" : "dark";
    }
    if (resolved === "light") {
      document.documentElement.style.background = "#f5f7fa";
      document.documentElement.style.colorScheme = "light";
    } else {
      document.documentElement.style.background = "#08111d";
      document.documentElement.style.colorScheme = "dark";
    }
  } catch(e) {}
})()`;

// Se un chunk JS non si carica (build vecchio dopo un deploy), ricarica la pagina
// una sola volta. Vanno intercettati DUE canali distinti, altrimenti l'auto-reload
// non scatta:
//  - resource error del <script src="/_next/static/..."> -> NON fa bubbling, va
//    ascoltato in fase di cattura (terzo argomento true);
//  - ChunkLoadError da import() dinamico -> arriva come unhandledrejection, non
//    come evento 'error' sincrono.
const chunkErrorScript = `(function(){
  function isChunkErr(msg, target){
    if (msg && (msg.indexOf('Loading chunk') !== -1 || msg.indexOf('ChunkLoadError') !== -1)) return true;
    if (target && target.tagName === 'SCRIPT' && target.src && target.src.indexOf('/_next/static/') !== -1) return true;
    return false;
  }
  function reloadOnce(){
    var key = 'nexus:chunkReload';
    var last = sessionStorage.getItem(key);
    var now = Date.now();
    if (!last || now - parseInt(last, 10) > 30000) {
      sessionStorage.setItem(key, String(now));
      window.location.reload();
    }
  }
  window.addEventListener('error', function(e){
    if (isChunkErr(e && e.message, e && e.target)) reloadOnce();
  }, true);
  window.addEventListener('unhandledrejection', function(e){
    var r = e && e.reason;
    var msg = r ? ((r.name || '') + ' ' + (r.message || '')) : '';
    if (isChunkErr(msg, null)) reloadOnce();
  });
})();`;

export default function RootLayout({ children }: { children: ReactNode }) {
  return (
    <html lang="en" className={jetBrainsMono.variable} suppressHydrationWarning>
      <head>
        <script dangerouslySetInnerHTML={{ __html: themeScript }} />
        <script dangerouslySetInnerHTML={{ __html: chunkErrorScript }} />
      </head>
      <body suppressHydrationWarning>
        <ThemeBody>{children}</ThemeBody>
      </body>
    </html>
  );
}
