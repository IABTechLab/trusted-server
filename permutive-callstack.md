# Permutive call stack analysis

(Analysis generated from HAR file `autoblog-permutive.har`)

### Overview

This HAR trace includes several network requests to **Permutive**, a data management and analytics platform used for audience segmentation, consent management, and advertising personalization. The trace shows that Autoblog’s site integrates Permutive both as a **CDN-loaded JavaScript library** and through **backend API endpoints** for event tracking and user synchronization.

### Step-by-step breakdown

#### 1. Script loading

* **URL example:** `https://cdn.permutive.com/autoblog/xyz123-web.js`
* **Purpose:** The browser first downloads a JavaScript SDK from the Permutive CDN. This script initializes the Permutive runtime within the page, sets up event listeners, and reads any stored identifiers or consent tokens from local storage or cookies.
* **What happens next:** Once loaded, the script queues initial events (page view, user session) and loads configuration from Permutive’s project settings.

#### 2. Initialization call

* **URL example:** `https://api.permutive.com/v2/projects/abc123/config`
* **Purpose:** Fetches configuration for the Autoblog project. This includes project ID, enabled modules (segmentation, identity sync, publisher integrations), and sampling rules.
* **Data sent:** Usually minimal—mostly the site’s project key, environment, and SDK version.
* **Data received:** A JSON configuration that defines which segments, events, and partner pixels should be active.

#### 3. Event collection

* **URL example:** `https://events.permutive.app/collect`
* **Purpose:** Sends event payloads such as `pageView`, `contentView`, or custom behavioral events triggered by the SDK.
* **Data sent:** Contains anonymous or pseudonymous identifiers (UUIDs or hashed IDs), timestamps, event names, and contextual metadata (page category, article ID, user device type, etc.).
* **Response:** Usually a 204 No Content—server simply acknowledges receipt.

#### 4. Identity synchronization

* **URL example:** `https://sync.permutive.com/v1/sync`
* **Purpose:** Syncs user IDs with external partners (e.g., The Trade Desk, Google, or other SSP/DSPs). This allows ad systems to align audience segments.
* **Data sent:** Includes a unique Permutive user ID and partner IDs if already known. Sometimes cookies like `_puid` or `_permutive-id` are referenced.
* **Response:** Often returns JSON mapping or triggers redirects to partner sync URLs.

#### 5. Segment updates

* **URL example:** `https://api.permutive.com/v1/segments`
* **Purpose:** Requests or updates which audience segments the current user belongs to based on prior behavior.
* **Data sent:** Permutive user ID, project ID, and event triggers.
* **Response:** A JSON array of active segment identifiers.

### Overall flow summary

1. **CDN script loads** and boots the SDK.
2. **Configuration fetched** to understand which events and integrations to run.
3. **Events collected** and sent to the `/collect` endpoint.
4. **Segments synced** and updated locally.
5. **Partner syncs** propagate IDs to external ad networks.

### Components involved

| Component               | Role                            | Notes                                       |
| ----------------------- | ------------------------------- | ------------------------------------------- |
| `cdn.permutive.com`     | Hosts JavaScript SDK            | Static script defining client logic         |
| `api.permutive.com`     | Configuration & segment updates | Used during initialization and segmentation |
| `events.permutive.app`  | Event collection endpoint       | Receives behavioral events                  |
| `sync.permutive.com`    | Identity synchronization        | Connects to partner ad systems              |
| Local Storage & Cookies | Client persistence              | Keeps user/session data between visits      |
