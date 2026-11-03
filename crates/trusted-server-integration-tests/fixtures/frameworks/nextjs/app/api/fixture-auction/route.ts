import { NextRequest, NextResponse } from "next/server";
import { FixtureAuctionControl } from "../../publisher-registration-real-gpt/provider-control";

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
  mode: ReturnType<FixtureAuctionControl["forPath"]>;
};

// Module state belongs to this isolated fixture container, not a browser bid map.
const exchanges: Exchange[] = [];
const control = new FixtureAuctionControl();

export async function GET() {
  return NextResponse.json({ exchanges, control: control.snapshot() });
}

export async function PUT(request: NextRequest) {
  try {
    return NextResponse.json({ control: control.set(await request.json()) });
  } catch {
    return NextResponse.json(
      { error: "fixture control must be exactly {noBid:boolean}" },
      { status: 400 },
    );
  }
}

export async function DELETE() {
  exchanges.length = 0;
  control.reset();
  return NextResponse.json({ exchanges, control: control.snapshot() });
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
  const mode = control.forPath(pagePath);
  const noBid = mode.noBid;
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
    mode,
  });
  console.log(
    JSON.stringify({ fixtureAuction: exchanges[exchanges.length - 1] }),
  );
  return NextResponse.json(response);
}
