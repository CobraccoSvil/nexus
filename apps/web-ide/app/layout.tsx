import type { ReactNode } from "react";
// @ts-expect-error - CSS side-effect import senza tipi
import "./globals.css";
// @ts-expect-error - xterm CSS import senza tipi
import "@xterm/xterm/css/xterm.css";
import { ThemeBody } from "./theme-body";

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
    <html lang="en" suppressHydrationWarning>
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
