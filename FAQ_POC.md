**What is Trusted Server POC?**
The  POC or proof of concept is to prove the technical feasibility of managing advertising for publishers on the server. It uses the edge cloud like Fastly, Akamai etc. to initiate ad requests, integrate prebid server, receive ad response and stitch the ad on the server before sending the page to the client browser. In addition it showcases the capabilities to maintain a key-value (kv) store to use critical data needed by a publisher for advertising and the capability to uniquely identify a user agent using a persistent id

**Is this ready for use by publishers?**
NO. It is NOT a version 1.0 and is NOT ready for use by a punisher in its current form.Its a proof of concept (POC). The purpose is to showcase technical feasibility of executing publisher  advertising functions server side and initiate industry effort to define and build an MVP or version 1.0 that is ready for use by publishers

**Why did Tech Lab build the Trusted Server POC?**
Today all of the publisher advertising is managed using third party providers, telemetry and capabilities available on the browser. New privacy initiatives and proposals by all major browsers are severely restricting most third party activities and telemetry used in advertising thus making it less and less effective, for e.g. third party cookie deprecation. Moreover, ad blocking is only growing worldwide. Managing ad requests on the edge cloud offers several advantages and better controls to the publisher, for e.g. device and user identification,enabling third parties with better control, managing privacy and improving page performance.

**Will the Trusted Server work for mobile in app advertising?**
The POC is designed for browsers and the initial trusted server focus is to support browser traffic. A second phase will focus on mobile apps. 

**Is the Trusted Server free?**
YES. The trusted server will be free and open source and offered under Apache 2.0 license terms.

**Does the POC integrate with my CMS?**
As of today, it doesn’t. This is a simple POC where we are hosting the very basic HTML/JS content inside the Trusted Server application itself. We will begin to release modules and reference architecture as the project moves forward. We’d also love contributions from our community on this front! 

**What is the vision for MVP or version 1.0?**
Tech Lab envisions the MVP to incorporate capabilities to make a valid Open RTB ad request using a prebid server and process the response for native, display and video ads. Our current plan for MVP version is that it can support addressability (user/device signals, ID solutions, other audience and targeting solutions), privacy management (CMP  execution, GPP and TCF strings), CMS integration, Publisher Ad server integration and prebid server integration. We look forward to the industry to help develop the MVP definition. 

**Will Tech Lab build all the capabilities?**
NOT all the capabilities. Tech Lab will build and support core services to enable existing publisher and adtech companies to build their own modules to support publisher advertising capabilities. 

**Does the Trusteed Server preclude using third party tags?**
No, it does not. You should be able to begin migrating certain modules and parts of your content and experience as you go, without a forklift upgrade. We plan to support server side tagging capabilities to enable third party support.

**Why are you only using two vendors in the POC?** 
Fastly and Equativ volunteered time and resources to us and they fit the technical needs and requirements for Trusted Server. For the sake of getting to market ASAP, we chose to double down on these two partners. We do not play favorites or have any financial incentive with these two companies and will begin implementing on other partners in the near future.  Any ad exchange supporting prebid server requests should already find support. We will prioritize modules for other edge cloud providers based on industry priorities

**How will this project be managed?**
Tech lab has created the ‘Trusted Server Taskforce’ to manage the requirements and roadmap for the project. We will maintain a core engineering team to build based on task force priorities and invite community contributions, especially third party providers to build their tags and modules for publishers.

**How will you support the publishers planning to implement?**
Yes! As of today we will be providing community support on a best effort basis as we scale up the architecture and use cases. We will do our best to respond to inquiries in the Github project as quickly as we can. 

**How will it work for medium size publishers using managed Word Press hosting (Ghost)?**
Yes. As long as your managed service provider can separate the edge from the CMS, They should be able to run trusted server. Some work may be necessary and we look to the community for contributions in defining and building for it.

**How will this comply with Privacy regulations?**
The trusted server will have modules to support Consent Management Providers (CMP) and send the GPP or TCF string as required in the ad request.

