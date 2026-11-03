// Local fixture state only. It changes actual OpenRTB provider responses, never GPT responses.
export class FixtureAuctionControl {
  private forcedNoBid = false;

  snapshot() {
    return { noBid: this.forcedNoBid };
  }

  set(input: unknown) {
    if (
      !input ||
      typeof input !== "object" ||
      Array.isArray(input) ||
      Object.keys(input).length !== 1 ||
      typeof (input as { noBid?: unknown }).noBid !== "boolean"
    )
      throw new Error("fixture control must be exactly {noBid:boolean}");
    this.forcedNoBid = (input as { noBid: boolean }).noBid;
    return this.snapshot();
  }

  reset() {
    this.forcedNoBid = false;
  }

  forPath(path: string) {
    const pathNoBid = path.endsWith("/no-bid");
    return {
      noBid: this.forcedNoBid || pathNoBid,
      forcedNoBid: this.forcedNoBid,
      source: this.forcedNoBid ? "control" : pathNoBid ? "path" : "default",
    };
  }
}
