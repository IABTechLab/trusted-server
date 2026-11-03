import RealGptPublisher, { type RealGptInventory } from "./RealGptPublisher";

export const dynamic = "force-dynamic";

export default function RealGptPage() {
  const raw = process.env.PUBLISHER_REGISTRATION_REAL_GPT_INVENTORY;
  if (!raw)
    throw new Error(
      "Dedicated real GPT inventory must be supplied by the acceptance launcher",
    );
  const inventory = JSON.parse(raw) as RealGptInventory & {
    authorizeGoogleTraffic?: boolean;
    dedicatedTestInventory?: boolean;
    gptScriptUrl?: string;
  };
  if (
    inventory.authorizeGoogleTraffic !== true ||
    inventory.dedicatedTestInventory !== true ||
    !inventory.gptScriptUrl
  ) {
    throw new Error("Dedicated real GPT inventory authorization is missing");
  }
  return (
    <>
      <script
        dangerouslySetInnerHTML={{
          __html: "window.googletag=window.googletag||{cmd:[]};",
        }}
      />
      <script async src={inventory.gptScriptUrl} />
      <RealGptPublisher
        inventory={inventory}
        legacy={process.env.PUBLISHER_REGISTRATION_REAL_GPT_MODE === "legacy"}
      />
    </>
  );
}
