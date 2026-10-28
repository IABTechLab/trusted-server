//! The `OpenRTB` 2.6 request the driver builds for every source that speaks
//! it, from the facts core routed and nothing else.

use error_stack::Report;
use serde_json::Map;
use trusted_server_core::auction::demand::{
    DemandTransport, ProviderAuctionInput, ProviderSlotInput, RequestFinalization, TransportHeaders,
};
use trusted_server_core::consent::ConsentContext;
use trusted_server_core::consent::jurisdiction::Jurisdiction;
use trusted_server_core::error::TrustedServerError;
use trusted_server_core::request_signing::SIGNING_VERSION;

use crate::demand::{
    DemandFieldPolicy, ImpressionExtension, OpenRtbDemand, RegsPolicy, RequestExtensions,
};
use crate::wire::{
    Banner, BidRequest, Device, Format, Geo, Imp, Publisher, Regs, RegsExt, Site, ToExt as _,
    TrustedServerExt, User, UserExt, format_eids, to_openrtb_i32,
};

/// The currency every request asks for and every bid is read in.
pub const DEFAULT_CURRENCY: &str = "USD";

/// The request extension the driver writes and no source may claim.
pub const TRUSTED_SERVER_EXT_KEY: &str = "trusted_server";

/// Builds one source's request from its routed input, or `None` when the
/// source is left with no impression to send.
///
/// Every standard field is built here from the routed facts, chosen through
/// the source's [`DemandFieldPolicy`]. The source then adds its own
/// extensions, and the request is signed when a signer is given.
///
/// # Errors
///
/// Returns an auction error when the source's own extensions cannot be built,
/// the source claims the extension the driver owns, or the signing input does
/// not bind the request's id.
pub fn build_request(
    demand: &dyn OpenRtbDemand,
    input: &ProviderAuctionInput,
    transport: DemandTransport<'_>,
    timeout_ms: u32,
    finalization: &RequestFinalization<'_>,
) -> Result<Option<BidRequest>, Report<TrustedServerError>> {
    let policy = demand.field_policy();
    let (mut request, slot_indices) =
        build_common_request(input, transport, demand, policy, timeout_ms);
    if request.imp.is_empty() {
        return Ok(None);
    }
    let omitted = {
        // The source sees the extension objects and the slot each impression
        // came from, and nothing else of the request.
        let BidRequest { ext, imp, .. } = &mut request;
        let impressions = imp
            .iter_mut()
            .zip(&slot_indices)
            .map(|(imp, &index)| ImpressionExtension {
                slot: &input.slots()[index],
                ext: &mut imp.ext,
                omitted: false,
            })
            .collect();
        let mut extensions = RequestExtensions {
            request: ext,
            impressions,
        };
        demand.augment_request(&mut extensions, input)?;
        extensions
            .impressions
            .iter()
            .map(|impression| impression.omitted)
            .collect::<Vec<bool>>()
    };
    // An impression the source left without demand goes, and so does a
    // request that has none left, so a demand source is never asked for
    // nothing.
    if omitted.iter().any(|&omitted| omitted) {
        let mut omitted = omitted.into_iter();
        request.imp.retain(|_| !omitted.next().unwrap_or(false));
    }
    if request.imp.is_empty() {
        return Ok(None);
    }
    if request
        .ext
        .as_ref()
        .is_some_and(|ext| ext.contains_key(TRUSTED_SERVER_EXT_KEY))
    {
        return Err(Report::new(TrustedServerError::Auction {
            message: format!(
                "Provider {} set ext.{TRUSTED_SERVER_EXT_KEY}, which only the driver writes",
                input.provider_id().as_str()
            ),
        }));
    }
    finalize_request(&mut request, policy, finalization)?;
    Ok(Some(request))
}

/// Builds every standard field of the request. The second value holds, for
/// each impression built, the index of the routed slot it came from.
fn build_common_request(
    input: &ProviderAuctionInput,
    transport: DemandTransport<'_>,
    demand: &dyn OpenRtbDemand,
    policy: DemandFieldPolicy,
    timeout_ms: u32,
) -> (BidRequest, Vec<usize>) {
    let common = input.common_request();
    let (slot_indices, imps): (Vec<usize>, Vec<Imp>) = input
        .slots()
        .iter()
        .enumerate()
        .filter_map(|(index, slot)| build_imp(slot, policy).map(|imp| (index, imp)))
        .unzip();
    let site_domain = demand.site_domain(&common.publisher.domain);
    let page = demand.site_page(common.publisher.page_url.as_deref(), &site_domain);
    let body_consent = demand.body_consent(common.user.consent.as_ref());
    let raw_tc = body_consent.and_then(|value| value.value(SCHEME_TCF).map(str::to_owned));
    let user = Some(User {
        id: common.user.id.clone(),
        consent: raw_tc.clone(),
        ext: UserExt {
            consent: raw_tc,
            consented_providers_settings: None,
            eids: common.user.eids.as_deref().map(format_eids),
        }
        .to_ext(),
        ..Default::default()
    });
    let language = normalized_language(transport.headers, policy);
    let device = common
        .device
        .as_ref()
        .map(|device| Device {
            ua: device.user_agent.clone(),
            ip: device.ip.clone(),
            geo: device.geo.as_ref().map(|geo| Geo {
                country: Some(geo.country.clone()),
                region: geo.region.clone(),
                city: Some(geo.city.clone()),
                lat: policy.precise_geo.then_some(geo.latitude),
                lon: policy.precise_geo.then_some(geo.longitude),
                metro: (geo.metro_code > 0).then(|| geo.metro_code.to_string()),
                r#type: Some(2),
                ..Default::default()
            }),
            dnt: transport.dnt,
            language: language.clone(),
            // Attributes the selected device module resolved, each absent
            // when nothing resolved it.
            devicetype: device.attributes.as_ref().and_then(|a| a.device_type),
            make: device.attributes.as_ref().and_then(|a| a.make.clone()),
            model: device.attributes.as_ref().and_then(|a| a.model.clone()),
            os: device.attributes.as_ref().and_then(|a| a.os.clone()),
            osv: device
                .attributes
                .as_ref()
                .and_then(|a| a.os_version.clone()),
            w: device.attributes.as_ref().and_then(|a| a.screen_width),
            h: device.attributes.as_ref().and_then(|a| a.screen_height),
            ..Default::default()
        })
        .or_else(|| {
            (transport.dnt.is_some() || language.is_some()).then_some(Device {
                dnt: transport.dnt,
                language,
                ..Default::default()
            })
        });

    let request = BidRequest {
        id: Some(common.id.clone()),
        imp: imps,
        site: Some(Site {
            domain: Some(site_domain.clone()),
            page,
            r#ref: policy
                .site_ref
                .then(|| header_string(transport.headers.referer()))
                .flatten(),
            publisher: Some(Publisher {
                domain: Some(site_domain),
                ..Default::default()
            }),
            ..Default::default()
        }),
        user,
        device,
        regs: build_regs(body_consent, policy.regs),
        test: policy.test.then_some(true),
        tmax: to_openrtb_i32(timeout_ms, "tmax", "config-first provider request"),
        cur: vec![DEFAULT_CURRENCY.to_string()],
        ..Default::default()
    };
    (request, slot_indices)
}

fn build_imp(slot: &ProviderSlotInput, policy: DemandFieldPolicy) -> Option<Imp> {
    let formats = slot
        .slot()
        .formats
        .iter()
        .filter_map(|format| {
            Some(Format {
                w: to_openrtb_i32(format.width, "format.w", "routed slot"),
                h: to_openrtb_i32(format.height, "format.h", "routed slot"),
                ..Default::default()
            })
            .filter(|value| value.w.is_some() && value.h.is_some())
        })
        .collect::<Vec<_>>();
    let first_width = formats.first()?.w;
    let first_height = formats.first()?.h;
    let primary_size = policy.primary_banner_size;
    Some(Imp {
        id: Some(slot.slot().id.clone()),
        banner: Some(Banner {
            format: formats,
            w: primary_size.then_some(first_width).flatten(),
            h: primary_size.then_some(first_height).flatten(),
            topframe: primary_size.then_some(false),
            ..Default::default()
        }),
        tagid: policy.imp_tagid.then(|| slot.slot().id.clone()),
        bidfloor: slot.slot().floor_price,
        bidfloorcur: slot
            .slot()
            .floor_price
            .map(|_| DEFAULT_CURRENCY.to_string()),
        secure: Some(true),
        ..Default::default()
    })
}

fn finalize_request(
    request: &mut BidRequest,
    policy: DemandFieldPolicy,
    finalization: &RequestFinalization<'_>,
) -> Result<(), Report<TrustedServerError>> {
    let request_id = request.id.as_deref().ok_or_else(|| {
        Report::new(TrustedServerError::Auction {
            message: "OpenRTB request ID must be fixed before signing".to_string(),
        })
    })?;
    if request_id != finalization.signing_params.request_id {
        return Err(Report::new(TrustedServerError::Auction {
            message: "OpenRTB signing params do not bind the fixed request ID".to_string(),
        }));
    }
    let trusted_server = if let Some(signer) = finalization.signer {
        let signature = signer.sign_request(&finalization.signing_params)?;
        Some(TrustedServerExt {
            version: Some(SIGNING_VERSION.to_string()),
            signature: Some(signature),
            kid: Some(signer.kid.clone()),
            request_host: Some(finalization.signing_params.request_host.clone()),
            request_scheme: Some(finalization.signing_params.request_scheme.clone()),
            ts: Some(finalization.signing_params.timestamp),
        })
    } else if policy.unsigned_request_identity {
        Some(TrustedServerExt {
            version: None,
            signature: None,
            kid: None,
            request_host: Some(finalization.signing_params.request_host.clone()),
            request_scheme: Some(finalization.signing_params.request_scheme.clone()),
            ts: None,
        })
    } else {
        None
    };
    if let Some(trusted_server) = trusted_server {
        let ext = request.ext.get_or_insert_with(Map::new);
        let serialized = serde_json::to_value(trusted_server).map_err(|error| {
            Report::new(TrustedServerError::Auction {
                message: format!("Failed to serialize Trusted Server extension: {error}"),
            })
        })?;
        ext.insert(TRUSTED_SERVER_EXT_KEY.to_string(), serialized);
    }
    Ok(())
}

/// The schemes whose signals the request carries in its own fields, by the
/// names the modules for them record. The strings are read through the
/// consent context's neutral interface here, and the crate for each scheme
/// answers the same question for a reader that depends on it.
const SCHEME_TCF: &str = "tcf";
const SCHEME_GPP: &str = "gpp";
const SCHEME_US_PRIVACY: &str = "us_privacy";

fn build_regs(consent: Option<&ConsentContext>, policy: RegsPolicy) -> Option<Regs> {
    let consent = consent?;
    let applies = consent.applies();
    let us_privacy = consent.value(SCHEME_US_PRIVACY).map(str::to_owned);
    let gpp = consent.value(SCHEME_GPP).map(str::to_owned);
    let gpp_sid = consent
        .record(SCHEME_GPP)
        .map(|record| record.sections().to_vec())
        .filter(|sections| !sections.is_empty());
    if policy == RegsPolicy::ApplicabilityBit {
        // Any admitted context produces regs and GDPR comes from the
        // applicability bit alone, without jurisdiction rules.
        let ext = RegsExt {
            gdpr: Some(u8::from(applies)),
            us_privacy: us_privacy.clone(),
            gpp: gpp.clone(),
            gpp_sid: gpp_sid.clone(),
        };
        return Some(Regs {
            coppa: None,
            gdpr: Some(applies),
            us_privacy,
            gpp,
            gpp_sid: gpp_sid
                .map(|ids| ids.into_iter().map(i32::from).collect())
                .unwrap_or_default(),
            ext: ext.to_ext(),
        });
    }

    // The jurisdiction policy sends no regs for an empty context and reads
    // GDPR from the applicability bit or from a GDPR jurisdiction.
    let has_data = applies || !consent.is_empty() || gpp_sid.is_some();
    if !has_data {
        return None;
    }
    let gdpr = if applies || matches!(consent.jurisdiction, Jurisdiction::Gdpr) {
        Some(true)
    } else if matches!(consent.jurisdiction, Jurisdiction::Unknown) {
        None
    } else {
        Some(false)
    };
    let ext = RegsExt {
        gdpr: gdpr.map(u8::from),
        us_privacy: us_privacy.clone(),
        gpp: gpp.clone(),
        gpp_sid: gpp_sid.clone(),
    };
    Some(Regs {
        coppa: None,
        gdpr,
        us_privacy,
        gpp,
        gpp_sid: gpp_sid
            .map(|ids| ids.into_iter().map(i32::from).collect())
            .unwrap_or_default(),
        ext: ext.to_ext(),
    })
}

fn normalized_language(headers: &TransportHeaders, policy: DemandFieldPolicy) -> Option<String> {
    let value = header_string(headers.accept_language())
        .and_then(|value| value.split(',').next().map(str::to_string))
        .and_then(|value| value.split(';').next().map(str::to_string))
        .and_then(|value| value.split('-').next().map(str::to_string))
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())?;
    match policy.language_max_bytes {
        None => Some(value),
        Some(max_bytes) => (value.len() <= max_bytes).then_some(value),
    }
}

fn header_string(value: Option<&http::HeaderValue>) -> Option<String> {
    value
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}
