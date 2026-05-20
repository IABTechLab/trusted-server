//! Platform-neutral Image Optimizer profile-table handling for asset routes.
//!
//! Asset routes accept small, publisher-defined query controls such as
//! `profile`, `ar`, `x`, and `y`. This module converts those controls into a
//! closed transformation set before the request reaches a platform adapter. It
//! does not pass arbitrary client query parameters through as Image Optimizer
//! options.
//!
//! The profile table supports shared base parameters, per-profile overrides,
//! optional aspect-ratio overrides, crop offset bucketing, and a debug bypass
//! parameter that disables image optimization for a single request.

use std::borrow::Cow;
use std::collections::HashSet;

use error_stack::Report;
use url::form_urlencoded;

use crate::error::TrustedServerError;
use crate::platform::{
    PlatformImageOptimizerCrop, PlatformImageOptimizerCropMode, PlatformImageOptimizerOptions,
    PlatformImageOptimizerParams,
};
use crate::settings::{
    ImageOptimizerCropOffsetsConfig, ImageOptimizerProfileSet, MissingCropOffsetMode,
    OriginQueryPolicy, ProxyAssetRoute, Settings, UnknownProfilePolicy,
};

/// Build Image Optimizer metadata for a route and request query.
///
/// The incoming query is read only as profile-table input. The asset proxy
/// applies the route origin-query policy separately before signing or sending
/// the upstream request.
///
/// Returns `Ok(None)` when the route has no enabled IO config or when the
/// configured debug parameter disables IO for this request.
///
/// # Errors
///
/// Returns a proxy/configuration error if the configured profile set is missing,
/// a profile parameter cannot be parsed, or the request references an unknown
/// profile in `reject` mode.
pub(crate) fn options_for_asset_request(
    settings: &Settings,
    route: &ProxyAssetRoute,
    query: &str,
) -> Result<Option<PlatformImageOptimizerOptions>, Report<TrustedServerError>> {
    let Some(route_config) = &route.image_optimizer else {
        return Ok(None);
    };
    if !route_config.enabled {
        return Ok(None);
    }

    let profile_set = settings
        .image_optimizer
        .profile_sets
        .get(&route_config.profile_set)
        .ok_or_else(|| {
            Report::new(TrustedServerError::Configuration {
                message: format!(
                    "proxy.asset_routes prefix `{}` references unknown image_optimizer profile_set `{}`",
                    route.prefix, route_config.profile_set
                ),
            })
        })?;

    if query_param_value(query, &profile_set.debug_param).as_deref() == Some("1") {
        return Ok(None);
    }

    let requested_profile = query_param_value(query, &profile_set.profile_param);
    let selected_profile = select_profile(profile_set, requested_profile.as_deref())?;
    let mut params = parse_param_string(&profile_set.base_params)?;
    let profile_params = profile_set
        .profiles
        .get(selected_profile)
        .expect("should select only configured image optimizer profiles");
    params.merge_from(parse_param_string(profile_params)?);

    apply_aspect_ratio_override(profile_set, selected_profile, query, &mut params);
    apply_crop_offsets(profile_set, query, &mut params);

    Ok(Some(
        PlatformImageOptimizerOptions::new(route_config.region.clone(), params)
            .with_preserve_query_string_on_origin_request(
                route.origin_query_policy() == OriginQueryPolicy::Preserve,
            ),
    ))
}

fn select_profile<'a>(
    profile_set: &'a ImageOptimizerProfileSet,
    requested_profile: Option<&str>,
) -> Result<&'a str, Report<TrustedServerError>> {
    let Some(requested_profile) = requested_profile.filter(|value| !value.is_empty()) else {
        return Ok(&profile_set.default_profile);
    };

    if let Some((configured_profile, _)) = profile_set.profiles.get_key_value(requested_profile) {
        return Ok(configured_profile.as_str());
    }

    match profile_set.unknown_profile {
        UnknownProfilePolicy::UseDefault => Ok(&profile_set.default_profile),
        UnknownProfilePolicy::Reject => Err(Report::new(TrustedServerError::BadRequest {
            message: format!("Unknown image profile: {requested_profile}"),
        })),
    }
}

fn query_param_value(query: &str, param_name: &str) -> Option<String> {
    form_urlencoded::parse(query.as_bytes())
        .find(|(key, _)| key.as_ref() == param_name)
        .map(|(_, value)| value.into_owned())
}

fn apply_aspect_ratio_override(
    profile_set: &ImageOptimizerProfileSet,
    selected_profile: &str,
    query: &str,
    params: &mut PlatformImageOptimizerParams,
) {
    let Some(aspect_config) = &profile_set.aspect_ratios else {
        return;
    };
    let Some(aspect_ratio) = query_param_value(query, &profile_set.aspect_ratio_param) else {
        return;
    };

    if !aspect_config
        .profiles
        .iter()
        .any(|profile| profile == selected_profile)
    {
        return;
    }
    if !aspect_config
        .allowed
        .iter()
        .any(|allowed| allowed == &aspect_ratio)
    {
        return;
    }
    let Some((width, height)) = parse_aspect_ratio_value(&aspect_ratio) else {
        return;
    };

    params.crop = Some(PlatformImageOptimizerCrop::aspect_ratio(width, height));
}

fn apply_crop_offsets(
    profile_set: &ImageOptimizerProfileSet,
    query: &str,
    params: &mut PlatformImageOptimizerParams,
) {
    let Some(offset_config) = &profile_set.crop_offsets else {
        return;
    };
    if !offset_config.enabled {
        return;
    }
    let Some(crop) = &mut params.crop else {
        return;
    };
    if !crop.is_bare_aspect_ratio() {
        return;
    }

    let x = query_param_value(query, &offset_config.x_param);
    let y = query_param_value(query, &offset_config.y_param);
    if x.is_none() && y.is_none() {
        if offset_config.when_missing == MissingCropOffsetMode::Smart {
            crop.mode = Some(PlatformImageOptimizerCropMode::Smart);
        }
        return;
    }

    crop.offset_x = Some(normalize_offset(x.as_deref(), offset_config));
    crop.offset_y = Some(normalize_offset(y.as_deref(), offset_config));
}

fn normalize_offset(value: Option<&str>, config: &ImageOptimizerCropOffsetsConfig) -> u32 {
    let Some(raw) = value else {
        return config.default;
    };
    let Ok(parsed) = raw.parse::<u32>() else {
        return config.default;
    };
    if parsed > 100 {
        return config.default;
    }

    let Some(first) = config.buckets.first().copied() else {
        return config.default;
    };
    for window in config.buckets.windows(2) {
        let current = window[0];
        let next = window[1];
        let midpoint = current + ((next - current) / 2);
        if parsed < midpoint {
            return current;
        }
    }
    config.buckets.last().copied().unwrap_or(first)
}

fn parse_param_string(
    params: &str,
) -> Result<PlatformImageOptimizerParams, Report<TrustedServerError>> {
    let mut parsed = PlatformImageOptimizerParams::default();
    if params.trim().is_empty() {
        return Ok(parsed);
    }

    for (key, value) in form_urlencoded::parse(params.as_bytes()) {
        let key = key.as_ref();
        let value = value.as_ref();
        match key {
            "format" => parsed.format = Some(parse_format(value)?),
            "quality" => parsed.quality = Some(parse_bounded_u32("quality", value, 0, 100)?),
            "resize-filter" => parsed.resize_filter = Some(parse_resize_filter(value)?),
            "width" => parsed.width = Some(parse_positive_u32("width", value)?),
            "height" => parsed.height = Some(parse_positive_u32("height", value)?),
            "crop" => parsed.crop = Some(parse_crop(value)?),
            unsupported => {
                return Err(Report::new(TrustedServerError::Configuration {
                    message: format!(
                        "unsupported image optimizer profile parameter `{unsupported}`"
                    ),
                }));
            }
        }
    }

    Ok(parsed)
}

fn parse_format(value: &str) -> Result<String, Report<TrustedServerError>> {
    let normalized = value.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "auto" | "avif" | "gif" | "jpeg" | "jpg" | "jxl" | "jpegxl" | "mp4" | "png" | "webp" => {
            Ok(normalized)
        }
        _ => Err(Report::new(TrustedServerError::Configuration {
            message: format!("unsupported image optimizer format `{value}`"),
        })),
    }
}

fn parse_resize_filter(value: &str) -> Result<String, Report<TrustedServerError>> {
    let normalized = value.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "nearest" | "bilinear" | "bicubic" | "lanczos2" | "lanczos3" => Ok(normalized),
        _ => Err(Report::new(TrustedServerError::Configuration {
            message: format!("unsupported image optimizer resize-filter `{value}`"),
        })),
    }
}

fn parse_positive_u32(name: &str, value: &str) -> Result<u32, Report<TrustedServerError>> {
    let parsed = value.parse::<u32>().map_err(|err| {
        Report::new(TrustedServerError::Configuration {
            message: format!("image optimizer `{name}` must be an integer: {err}"),
        })
    })?;
    if parsed == 0 {
        return Err(Report::new(TrustedServerError::Configuration {
            message: format!("image optimizer `{name}` must be greater than zero"),
        }));
    }
    Ok(parsed)
}

fn parse_bounded_u32(
    name: &str,
    value: &str,
    min: u32,
    max: u32,
) -> Result<u32, Report<TrustedServerError>> {
    let parsed = value.parse::<u32>().map_err(|err| {
        Report::new(TrustedServerError::Configuration {
            message: format!("image optimizer `{name}` must be an integer: {err}"),
        })
    })?;
    if parsed < min || parsed > max {
        return Err(Report::new(TrustedServerError::Configuration {
            message: format!("image optimizer `{name}` must be in {min}..={max}"),
        }));
    }
    Ok(parsed)
}

fn parse_crop(value: &str) -> Result<PlatformImageOptimizerCrop, Report<TrustedServerError>> {
    let mut parts = value.split(',');
    let ratio = parts.next().unwrap_or_default();
    let (width, height) = parse_crop_ratio(ratio)?;
    let mut crop = PlatformImageOptimizerCrop::aspect_ratio(width, height);
    let mut seen_suffixes = HashSet::new();

    for suffix in parts {
        if suffix == "smart" {
            crop.mode = Some(PlatformImageOptimizerCropMode::Smart);
            seen_suffixes.insert(Cow::Borrowed("smart"));
            continue;
        }
        if let Some(offset) = suffix.strip_prefix("offset-x") {
            crop.offset_x = Some(parse_bounded_u32("crop offset-x", offset, 0, 100)?);
            seen_suffixes.insert(Cow::Borrowed("offset-x"));
            continue;
        }
        if let Some(offset) = suffix.strip_prefix("offset-y") {
            crop.offset_y = Some(parse_bounded_u32("crop offset-y", offset, 0, 100)?);
            seen_suffixes.insert(Cow::Borrowed("offset-y"));
            continue;
        }
        return Err(Report::new(TrustedServerError::Configuration {
            message: format!("unsupported image optimizer crop suffix `{suffix}`"),
        }));
    }

    if crop.mode.is_some() && (crop.offset_x.is_some() || crop.offset_y.is_some()) {
        return Err(Report::new(TrustedServerError::Configuration {
            message: "image optimizer crop cannot combine smart mode with explicit offsets"
                .to_string(),
        }));
    }
    if seen_suffixes.contains(&Cow::Borrowed("offset-x"))
        != seen_suffixes.contains(&Cow::Borrowed("offset-y"))
    {
        return Err(Report::new(TrustedServerError::Configuration {
            message: "image optimizer crop offsets must include both offset-x and offset-y"
                .to_string(),
        }));
    }

    Ok(crop)
}

fn parse_crop_ratio(value: &str) -> Result<(u32, u32), Report<TrustedServerError>> {
    let (width, height) = value.split_once(':').ok_or_else(|| {
        Report::new(TrustedServerError::Configuration {
            message: format!("image optimizer crop `{value}` must look like `width:height`"),
        })
    })?;
    let width = parse_positive_u32("crop width", width)?;
    let height = parse_positive_u32("crop height", height)?;
    Ok((width, height))
}

fn parse_aspect_ratio_value(value: &str) -> Option<(u32, u32)> {
    let (width, height) = value.split_once('-')?;
    let width = width.parse::<u32>().ok()?;
    let height = height.parse::<u32>().ok()?;
    if width == 0 || height == 0 {
        return None;
    }
    Some((width, height))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{
        ImageOptimizerAspectRatioConfig, ImageOptimizerCropOffsetsConfig, ImageOptimizerProfileSet,
    };

    fn profile_set() -> ImageOptimizerProfileSet {
        let mut profiles = std::collections::HashMap::new();
        profiles.insert("default".to_string(), "width=1920".to_string());
        profiles.insert("medium".to_string(), "format=auto&width=828".to_string());
        ImageOptimizerProfileSet {
            base_params: "quality=70&resize-filter=bicubic".to_string(),
            default_profile: "default".to_string(),
            unknown_profile: UnknownProfilePolicy::UseDefault,
            profile_param: "profile".to_string(),
            aspect_ratio_param: "ar".to_string(),
            debug_param: "_io_debug".to_string(),
            profiles,
            aspect_ratios: Some(ImageOptimizerAspectRatioConfig {
                allowed: vec!["1-1".to_string(), "16-9".to_string()],
                profiles: vec!["medium".to_string()],
            }),
            crop_offsets: Some(ImageOptimizerCropOffsetsConfig {
                enabled: true,
                x_param: "x".to_string(),
                y_param: "y".to_string(),
                buckets: vec![10, 30, 50, 70, 90],
                default: 50,
                when_missing: MissingCropOffsetMode::Smart,
            }),
        }
    }

    #[test]
    fn profile_conversion_adds_aspect_ratio_and_smart_crop() {
        let set = profile_set();
        let selected = select_profile(&set, Some("medium")).expect("should select profile");
        let mut params = parse_param_string(&set.base_params).expect("should parse base params");
        params.merge_from(
            parse_param_string(set.profiles.get(selected).expect("should get profile"))
                .expect("should parse profile params"),
        );
        apply_aspect_ratio_override(&set, selected, "profile=medium&ar=1-1", &mut params);
        apply_crop_offsets(&set, "profile=medium&ar=1-1", &mut params);

        assert_eq!(params.quality, Some(70));
        assert_eq!(params.resize_filter.as_deref(), Some("bicubic"));
        assert_eq!(params.format.as_deref(), Some("auto"));
        assert_eq!(params.width, Some(828));
        let crop = params.crop.expect("should add crop");
        assert_eq!((crop.width, crop.height), (1, 1));
        assert_eq!(crop.mode, Some(PlatformImageOptimizerCropMode::Smart));
    }

    #[test]
    fn profile_conversion_buckets_offsets() {
        let set = profile_set();
        let selected = select_profile(&set, Some("medium")).expect("should select profile");
        let mut params = parse_param_string("width=828").expect("should parse params");
        apply_aspect_ratio_override(
            &set,
            selected,
            "profile=medium&ar=16-9&x=79&y=bad",
            &mut params,
        );
        apply_crop_offsets(&set, "profile=medium&ar=16-9&x=79&y=bad", &mut params);

        let crop = params.crop.expect("should add crop");
        assert_eq!((crop.offset_x, crop.offset_y), (Some(70), Some(50)));
    }
}
