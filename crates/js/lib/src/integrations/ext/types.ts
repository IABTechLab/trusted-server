// Minimal OpenRTB response typing (used by the Prebid extension)
export interface OpenRtbBid {
  impid?: string;
  adm?: string;
  [key: string]: unknown;
}
export interface OpenRtbSeatBid {
  bid?: OpenRtbBid[] | null;
}
export interface OpenRtbBidResponse {
  seatbid?: OpenRtbSeatBid[] | null;
}
