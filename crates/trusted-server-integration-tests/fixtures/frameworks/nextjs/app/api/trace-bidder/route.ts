import { NextResponse } from 'next/server'

export const dynamic = 'force-dynamic'

let requestCount = 0
let controlledMode = 'empty'

/** Counts actual HTTP invocations without retaining bidder request bodies. */
export async function GET() {
  return NextResponse.json({ requests: requestCount })
}

/** Selects one bounded scenario for the next real server-side auctions. */
export async function PUT(request: Request) {
  let control: unknown
  try {
    control = await request.json()
  } catch {
    return NextResponse.json({ error: 'invalid fixture mode' }, { status: 400 })
  }
  if (
    typeof control !== 'object' ||
    control === null ||
    !('mode' in control) ||
    typeof control.mode !== 'string' ||
    !['selected', 'empty', 'error'].includes(control.mode)
  ) {
    return NextResponse.json({ error: 'invalid fixture mode' }, { status: 400 })
  }
  controlledMode = control.mode
  return NextResponse.json({ mode: controlledMode })
}

/** Controlled fictional OpenRTB bidder reached by the real Rust HTTP client. */
export async function POST(request: Request) {
  const auction = await request.json()
  requestCount += 1
  const mode =
    new URL(
      auction.site?.page || 'https://publisher.example.com/'
    ).searchParams.get('trace_fixture') || controlledMode
  if (mode === 'error') {
    return NextResponse.json(
      { error: 'controlled fixture failure' },
      { status: 503 }
    )
  }
  const bids =
    mode === 'selected' && Array.isArray(auction.imp)
      ? auction.imp.map(
          (
            impression: {
              id: string
              banner?: { format?: { w: number; h: number }[] }
            },
            index: number
          ) => ({
            id: `example-bid-${index}`,
            impid: impression.id,
            price: 1,
            w: impression.banner?.format?.[0]?.w || 300,
            h: impression.banner?.format?.[0]?.h || 250,
            crid: 'example-creative',
            adm: '<!doctype html><html><body><div class="marker">Trace fixture creative</div><script>requestAnimationFrame(()=>top.postMessage({type:"fixture-creative-ready",label:"Trace fixture creative"},"*"));</script></body></html>',
          })
        )
      : []
  return NextResponse.json({
    id: auction.id,
    cur: 'USD',
    seatbid: bids.length ? [{ seat: 'example-bidder', bid: bids }] : [],
  })
}
