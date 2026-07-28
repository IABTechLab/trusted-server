"use client";

import Link from "next/link";
import { useState } from "react";

export default function GptDiagnosticsFixture() {
    const [slotVersion, setSlotVersion] = useState(1);
    const [duplicateId, setDuplicateId] = useState(false);
    const [showPrimarySlot, setShowPrimarySlot] = useState(true);

    return (
        <main style={{ minHeight: "1800px", padding: "24px" }}>
            <h1>GPT diagnostics fixture</h1>
            <p>Controlled fictional GPT slots for browser integration tests.</p>
            <nav aria-label="Fixture navigation">
                <Link href="/">Home</Link>
            </nav>

            <div
                style={{
                    display: "flex",
                    flexWrap: "wrap",
                    gap: "8px",
                    margin: "16px 0",
                }}
            >
                <button
                    type="button"
                    onClick={() => setSlotVersion((version) => version + 1)}
                >
                    Replace primary slot
                </button>
                <button
                    type="button"
                    onClick={() => setShowPrimarySlot((shown) => !shown)}
                >
                    Toggle primary slot
                </button>
                <button
                    type="button"
                    onClick={() => setDuplicateId((duplicated) => !duplicated)}
                >
                    Toggle duplicate ID
                </button>
                <button
                    type="button"
                    onClick={() =>
                        document
                            .getElementById("trusted-server-gpt-diagnostics")
                            ?.remove()
                    }
                >
                    Remove diagnostics host
                </button>
            </div>

            {showPrimarySlot ? (
                <div
                    key={slotVersion}
                    id="gpt-diagnostics-slot-primary"
                    data-slot-version={slotVersion}
                    style={{
                        width: "300px",
                        height: "250px",
                        border: "2px solid #2563eb",
                        background: "#dbeafe",
                    }}
                >
                    Primary fictional slot
                </div>
            ) : null}

            {duplicateId ? (
                <div
                    id="gpt-diagnostics-slot-primary"
                    style={{
                        width: "300px",
                        height: "100px",
                        border: "2px solid #dc2626",
                    }}
                >
                    Duplicate fixture ID
                </div>
            ) : null}

            <div style={{ height: "900px" }} aria-hidden="true" />

            <div
                id="gpt-diagnostics-slot-secondary"
                style={{
                    width: "728px",
                    maxWidth: "100%",
                    height: "90px",
                    border: "2px solid #16a34a",
                    background: "#dcfce7",
                }}
            >
                Secondary fictional slot
            </div>
        </main>
    );
}
