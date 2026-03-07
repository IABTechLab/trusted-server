import { NextResponse } from "next/server";

export async function GET() {
  return NextResponse.json({
    users: [
      { id: 1, name: "Alice", email: "alice@example.com" },
      { id: 2, name: "Bob", email: "bob@example.com" },
    ],
    metadata: { version: "1.0.0" },
  });
}
