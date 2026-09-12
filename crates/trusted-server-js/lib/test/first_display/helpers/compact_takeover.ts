export function failedFirstDisplayTakeover(releaseId: string) {
  const projectionDigest = 'b'.repeat(64);
  const integrationConfigDigest = 'c'.repeat(64);
  const projection = {
    version: 1,
    auction: {
      version: 1,
      auctionId: 'initial',
      results: [{ slot: 'slot-1', outcome: 'failed', reason: 'internal_error' }],
    },
    slots: [
      {
        slot: 'slot-1',
        gamUnitPath: '/123/slot-1',
        divId: 'div-1',
        formats: [[300, 250]],
        targeting: {},
      },
    ],
    bids: [],
  };
  return {
    capture: {
      captureVersion: 1,
      releaseId,
      generation: 1,
      data: [
        projectionDigest,
        integrationConfigDigest,
        ['first_display'],
        [['failed', 'internal_error', null, null]],
        [],
        [],
        [],
        [],
        [[], 0, 0],
        [0, null, 0, 0],
        [2, 0, 1, 1],
        [],
        1,
        2,
      ],
      mutationRevision: 0,
      identityCount: 0,
    },
    outline: {
      version: 1,
      releaseId,
      generation: 1,
      projectionDigest,
      integrationConfigDigest,
      slices: ['first_display'],
      slotCount: 1,
      outcomeCount: 1,
      capabilities: [],
      objectKinds: [],
    },
    boot: {
      abi: 1,
      releaseId,
      manifest: {},
      auctionProjection: projection,
      integrations: {},
      creative: {},
      diagnostics: {},
    },
  } as const;
}

export function acceptedGptFirstDisplayTakeover(
  releaseId: string,
  reservationId: string,
  parserState: 'valid' | 'invalid' | 'absent',
  gamAttributionEnabled: boolean
) {
  const projectionDigest = 'b'.repeat(64);
  const integrationConfigDigest = 'c'.repeat(64);
  const slices =
    parserState === 'absent'
      ? (['first_display'] as const)
      : (['first_display', 'gpt_initial'] as const);
  const projection = {
    version: 1,
    auction: {
      version: 1,
      auctionId: 'initial',
      results: [{ slot: 'takeover-slot', outcome: 'winner', candidateId: 'AAAAAAAAAAAA' }],
    },
    slots: [
      {
        slot: 'takeover-slot',
        gamUnitPath: '/123/takeover-slot',
        divId: 'takeover-slot',
        formats: [[300, 250]],
        targeting: {},
      },
    ],
    bids: [
      {
        candidateId: 'AAAAAAAAAAAA',
        slot: 'takeover-slot',
        provider: 'trusted',
        upstreamBidId: 'bid-1',
        cpm: 1,
        currency: 'USD',
        targeting: {},
        rendererReservationId: reservationId,
        renderSource: {
          type: 'adm',
          version: 1,
          adm: '<div>creative</div>',
          width: 300,
          height: 250,
        },
      },
    ],
  };
  const cycles = [
    {
      nextCycleOrdinal: 2,
      quarantines: [],
      records: [
        {
          ordinal: 1,
          responseIdentifier: 'response-one',
          seen: ['slotRequested', 'slotRenderEnded'],
          state: 'completed',
        },
      ],
      slotId: 'takeover-slot',
      token: 'gt1_1',
      unknownPriorCycle: false,
    },
  ];
  return {
    capture: {
      captureVersion: 1,
      releaseId,
      generation: 1,
      data: [
        projectionDigest,
        integrationConfigDigest,
        slices,
        [['accepted', null, 2, 1]],
        [['takeover-slot', 'takeover-slot', 'publisher', [], 'gt1_1']],
        [],
        [[null, null, 'takeover-slot', 'gpt_adm', 'trusted_server', reservationId]],
        parserState === 'valid'
          ? [
              [
                'gpt_initial',
                [
                  ['gam', gamAttributionEnabled],
                  ['v', 1],
                ],
              ],
            ]
          : [],
        [[], 0, 0],
        [1, 2, 3, 4],
        [2, 0, 2, 1],
        cycles,
        2,
        2,
      ],
      mutationRevision: 0,
      identityCount: 2,
    },
    outline: {
      version: 1,
      releaseId,
      generation: 1,
      projectionDigest,
      integrationConfigDigest,
      slices,
      slotCount: 1,
      outcomeCount: 1,
      capabilities: [],
      objectKinds: ['gpt_slot', 'dom_artifact'],
    },
    boot: {
      abi: 1,
      releaseId,
      manifest: {},
      auctionProjection: projection,
      integrations: {},
      creative: {},
      diagnostics: {},
    },
  } as const;
}
