import { NextRequest, NextResponse } from "next/server";

export async function GET(request: NextRequest) {
  const url = new URL(request.url);
  const backendUrl = `http://localhost:4000/api/admin/users${url.search}`;

  console.log("API proxy GET:", backendUrl);
  console.log("Cookie:", request.headers.get("cookie"));

  try {
    const response = await fetch(backendUrl, {
      method: "GET",
      headers: {
        "Cookie": request.headers.get("cookie") || "",
        "Content-Type": "application/json",
      },
    });

    console.log("Backend response status:", response.status);

    // Handle empty responses (like 401 Unauthorized with no body)
    if (response.status === 204 || response.headers.get("content-length") === "0") {
      return NextResponse.json(null, { status: response.status });
    }

    const data = await response.json();
    console.log("Backend data:", data);
    return NextResponse.json(data, { status: response.status });
  } catch (error) {
    console.error("API proxy error:", error);
    return NextResponse.json(
      { error: "Internal server error", details: String(error) },
      { status: 500 }
    );
  }
}

// Punto unico per PUT/DELETE (regola L): stesso forwarding, gestione 204
// e status passthrough; il body viene letto solo per la PUT.
async function forwardUsersRequest(
  request: NextRequest,
  method: "PUT" | "DELETE",
): Promise<NextResponse> {
  const url = new URL(request.url);
  const backendUrl = `http://localhost:4000/api/admin/users${url.search}`;
  const body = method === "PUT" ? JSON.stringify(await request.json()) : undefined;

  try {
    const response = await fetch(backendUrl, {
      method,
      headers: {
        "Cookie": request.headers.get("cookie") || "",
        "Content-Type": "application/json",
      },
      body,
    });

    // Handle empty responses
    if (response.status === 204 || response.headers.get("content-length") === "0") {
      return NextResponse.json(null, { status: response.status });
    }

    const data = await response.json();
    return NextResponse.json(data, { status: response.status });
  } catch (error) {
    console.error("API proxy error:", error);
    return NextResponse.json(
      { error: "Internal server error" },
      { status: 500 }
    );
  }
}

export async function PUT(request: NextRequest) {
  return forwardUsersRequest(request, "PUT");
}

export async function DELETE(request: NextRequest) {
  return forwardUsersRequest(request, "DELETE");
}
