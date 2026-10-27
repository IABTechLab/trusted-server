import RegisteredPublisher from "./RegisteredSlot";

// The fictional document never reads cookies. Rust assembles per-reader bids.
export const dynamic = "force-static";

export default function PublisherRegistrationPage() {
  return <RegisteredPublisher />;
}
