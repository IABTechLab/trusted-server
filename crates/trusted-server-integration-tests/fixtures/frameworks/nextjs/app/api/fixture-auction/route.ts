import { NextRequest, NextResponse } from "next/server";

export const dynamic = "force-dynamic";
export const runtime = "nodejs";

type Impression = {
  id: string;
  banner?: { format?: Array<{ w: number; h: number }> };
  ext?: { prebid?: { bidder?: Record<string, unknown> } };
};
type AuctionRequest = {
  id: string;
  imp: Impression[];
  tmax?: number;
  site?: { page?: string };
};
type Exchange = {
  path: string;
  request: AuctionRequest;
  response: object;
};

// Module state belongs to this isolated fixture container, not a browser bid map.
const exchanges: Exchange[] = [];

export async function GET() {
  return NextResponse.json({ exchanges });
}

export async function DELETE() {
  exchanges.length = 0;
  return NextResponse.json({ exchanges });
}

export async function POST(request: NextRequest) {
  const auction = (await request.json()) as AuctionRequest;
  if (
    typeof auction.id !== "string" ||
    !Array.isArray(auction.imp) ||
    auction.imp.length === 0 ||
    auction.imp.some(
      (imp) =>
        typeof imp.id !== "string" ||
        !imp.ext?.prebid?.bidder?.["fixture-bidder"] ||
        !imp.banner?.format?.some(({ w, h }) => w === 300 && h === 250),
    )
  ) {
    return NextResponse.json(
      { error: "invalid fixture OpenRTB request" },
      { status: 400 },
    );
  }

  const pagePath = new URL(
    auction.site?.page || "https://publisher.example.com/",
  ).pathname;
  const noBid = pagePath.endsWith("/no-bid");
  const height = pagePath.endsWith("/tall") ? 600 : 250;
  const response = {
    id: auction.id,
    cur: "USD",
    seatbid: noBid
      ? []
      : [
          {
            seat: "fixture-bidder",
            bid: auction.imp.map((imp) => ({
              id: `fixture-win-${imp.id}`,
              impid: imp.id,
              price: 1.25,
              adm: '<div data-fixture-creative="winner">Fixture publisher registration winner</div>',
              adomain: ["advertiser.example.com"],
              crid: `fixture-creative-300x${height}`,
              w: 300,
              h: height,
              ext: { prebid: { type: "banner", bidder: "fixture-bidder" } },
            })),
          },
        ],
  };
  exchanges.push({
    path: request.nextUrl.pathname,
    request: auction,
    response,
  });
  console.log(
    JSON.stringify({ fixtureAuction: exchanges[exchanges.length - 1] }),
  );
  return NextResponse.json(response);
}
