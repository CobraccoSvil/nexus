import { NextRequest, NextResponse } from "next/server";

export function middleware(request: NextRequest) {
  const token = request.cookies.get("token")?.value;
  const { pathname } = request.nextUrl;

  // Per le chiamate API/auth/neural/nexus: inietta Authorization: Bearer dal cookie token
  // Il gateway richiede Bearer header, non il cookie raw
  if (
    pathname.startsWith("/api/") ||
    pathname.startsWith("/auth/") ||
    pathname.startsWith("/neural/") ||
    pathname.startsWith("/nexus/")
  ) {
    if (token && !request.headers.get("authorization")) {
      const headers = new Headers(request.headers);
      headers.set("authorization", `Bearer ${token}`);
      return NextResponse.next({ request: { headers } });
    }
    return NextResponse.next();
  }

  // Allow login page and static assets
  if (pathname === "/login" || pathname.startsWith("/_next") || pathname.startsWith("/favicon")) {
    return NextResponse.next();
  }

  // Landing page: authenticated users go to /ide, others see landing.
  // ?site bypassa il redirect per mostrare la landing anche agli utenti loggati.
  if (pathname === "/") {
    if (token && !request.nextUrl.searchParams.has("site")) {
      return NextResponse.redirect(new URL("/ide", request.url));
    }
    return NextResponse.next();
  }

  // Redirect to login if no token
  if (!token) {
    const loginUrl = new URL("/login", request.url);
    return NextResponse.redirect(loginUrl);
  }

  return NextResponse.next();
}

export const config = {
  matcher: ["/((?!_next/static|_next/image|favicon.ico|api/).*)"],
};
