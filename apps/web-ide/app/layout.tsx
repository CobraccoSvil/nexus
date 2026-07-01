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

// Se un chunk JS non si carica (build vecchio in cache), ricarica la pagina una sola volta
const chunkErrorScript = `(function(){
  window.addEventListener('error', function(e) {
    if (e && e.message && e.message.indexOf('Loading chunk') !== -1) {
      var key = 'nexus:chunkReload';
      var last = sessionStorage.getItem(key);
      var now = Date.now();
      if (!last || now - parseInt(last, 10) > 30000) {
        sessionStorage.setItem(key, String(now));
        window.location.reload();
      }
    }
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
