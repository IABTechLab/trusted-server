//! Image Optimizer profile-table handling for asset routes.
//!
//! Asset routes accept small, publisher-defined query controls such as
//! `profile`, `ar`, `x`, and `y`. This module resolves those controls into a
//! closed transformation set, an [`ImageOptimizerProfile`], and maps it to the
//! `EdgeZero` options the final asset request carries. It does not pass
//! arbitrary client query parameters through as Image Optimizer options.
//!
//! The profile table supports shared base parameters, per-profile overrides,
//! optional aspect-ratio overrides, crop offset bucketing, and a debug bypass
//! parameter that disables image optimization for a single request.

use edgezero_core::error::EdgeError;
use edgezero_core::outbound::{
    OutboundCropFocus, OutboundImageFormat, OutboundImageOptimizerOptions,
    OutboundImageOptimizerRegion, OutboundResizeFilter,
};
use error_stack::Report;
use url::form_urlencoded;

use crate::error::TrustedServerError;
use crate::settings::{
    ImageOptimizerCropOffsetsConfig, ImageOptimizerProfileSet, MissingCropOffsetMode,
    OriginQueryPolicy, ProxyAssetRoute, Settings, UnknownProfilePolicy,
};

/// An Image Optimizer profile resolved for one asset request.
///
/// It holds the selected profile's parameters over the profile set's base
/// parameters, with the request's aspect ratio and crop offsets applied, plus
/// the route's region and origin query policy. [`image_optimizer_options`] maps
/// it to the options the asset request carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ImageOptimizerProfile {
    region: String,
    preserve_origin_query: bool,
    params: ImageOptimizerParams,
}

/// The transformation parameters a profile table can set.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct ImageOptimizerParams {
    format: Option<OutboundImageFormat>,
    quality: Option<u32>,
    resize_filter: Option<OutboundResizeFilter>,
    width: Option<u32>,
    height: Option<u32>,
    crop: Option<ImageOptimizerCrop>,
}

impl ImageOptimizerParams {
    /// Merges `other` into these parameters, with `other` taking precedence.
    fn merge_from(&mut self, other: Self) {
        if other.format.is_some() {
            self.format = other.format;
        }
        if other.quality.is_some() {
            self.quality = other.quality;
        }
        if other.resize_filter.is_some() {
            self.resize_filter = other.resize_filter;
        }
        if other.width.is_some() {
            self.width = other.width;
        }
        if other.height.is_some() {
            self.height = other.height;
        }
        if other.crop.is_some() {
            self.crop = other.crop;
        }
    }
}

/// An aspect-ratio crop and where it focuses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ImageOptimizerCrop {
    width: u32,
    height: u32,
    focus: ImageOptimizerCropFocus,
}

impl ImageOptimizerCrop {
    /// A bare aspect-ratio crop around the default focus.
    fn aspect_ratio(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            focus: ImageOptimizerCropFocus::Default,
        }
    }
}

/// Where a crop focuses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ImageOptimizerCropFocus {
    /// Image Optimizer's default focus, for a bare aspect ratio.
    Default,
    /// Image Optimizer's smart crop.
    Smart,
    /// Crop offsets, as percentages of the image.
    Offset { x: u32, y: u32 },
}

/// Resolves the Image Optimizer profile for a route and request query.
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
pub(crate) fn profile_for_asset_request(
    settings: &Settings,
    route: &ProxyAssetRoute,
    query: &str,
) -> Result<Option<ImageOptimizerProfile>, Report<TrustedServerError>> {
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
        .ok_or_else(|| {
            Report::new(TrustedServerError::Configuration {
                message: format!(
                    "image_optimizer.profile_sets `{}` selected profile `{selected_profile}` is not defined",
                    route_config.profile_set
                ),
            })
        })?;
    params.merge_from(parse_param_string(profile_params)?);

    apply_aspect_ratio_override(profile_set, selected_profile, query, &mut params);
    apply_crop_offsets(profile_set, query, &mut params);

    Ok(Some(ImageOptimizerProfile {
        region: route_config.region.clone(),
        preserve_origin_query: route.origin_query_policy() == OriginQueryPolicy::Preserve,
        params,
    }))
}

/// Maps a resolved profile to the `EdgeZero` Image Optimizer options its asset
/// request carries.
///
/// Trusted Server's region aliases, such as `us-east-1`, map to `EdgeZero`'s
/// seven regions through [`image_optimizer_region`]. The route's origin query
/// policy maps to `preserve_origin_query`.
///
/// # Errors
///
/// Returns [`EdgeError::BadRequest`] for an unknown region, or for a quality,
/// dimension, or crop value that Image Optimizer does not accept. Settings
/// validation rejects all of them, so the asset proxy serves the image
/// unoptimized if this ever fails.
pub(crate) fn image_optimizer_options(
    profile: &ImageOptimizerProfile,
) -> Result<OutboundImageOptimizerOptions, EdgeError> {
    let region = image_optimizer_region(&profile.region)
        .ok_or_else(|| EdgeError::bad_request("unsupported image optimizer region"))?;
    let params = &profile.params;
    let mut options = OutboundImageOptimizerOptions::new()
        .region(region)
        .preserve_origin_query(profile.preserve_origin_query);
    if let Some(format) = params.format {
        options = options.format(format);
    }
    if let Some(quality) = params.quality {
        options = options.quality(u8::try_from(quality).map_err(|_| {
            EdgeError::bad_request("image optimizer quality must be between 0 and 100")
        })?)?;
    }
    if let Some(resize_filter) = params.resize_filter {
        options = options.resize_filter(resize_filter);
    }
    if let Some(width) = params.width {
        options = options.width(width)?;
    }
    if let Some(height) = params.height {
        options = options.height(height)?;
    }
    if let Some(crop) = params.crop {
        let focus = match crop.focus {
            ImageOptimizerCropFocus::Default => OutboundCropFocus::Default,
            ImageOptimizerCropFocus::Smart => OutboundCropFocus::Smart,
            ImageOptimizerCropFocus::Offset { x, y } => OutboundCropFocus::Percent {
                x: crop_percentage(x)?,
                y: crop_percentage(y)?,
            },
        };
        options = options.crop(crop.width, crop.height, focus)?;
    }

    Ok(options)
}

/// Maps a configured Image Optimizer region, or one of its aliases, to an
/// `EdgeZero` region.
///
/// Matching ignores surrounding space, case, and `-` versus `_`, so `us-east-1`
/// and `US_EAST` both name [`OutboundImageOptimizerRegion::UsEast`]. Settings
/// validation accepts exactly the regions this maps.
pub(crate) fn image_optimizer_region(region: &str) -> Option<OutboundImageOptimizerRegion> {
    match region
        .trim()
        .to_ascii_lowercase()
        .replace('-', "_")
        .as_str()
    {
        "us_east" | "us_east_1" => Some(OutboundImageOptimizerRegion::UsEast),
        "us_central" | "us_central_1" => Some(OutboundImageOptimizerRegion::UsCentral),
        "us_west" | "us_west_1" | "us_west_2" => Some(OutboundImageOptimizerRegion::UsWest),
        "eu_central" | "eu_central_1" => Some(OutboundImageOptimizerRegion::EuCentral),
        "eu_west" | "eu_west_1" => Some(OutboundImageOptimizerRegion::EuWest),
        "asia" => Some(OutboundImageOptimizerRegion::Asia),
        "australia" => Some(OutboundImageOptimizerRegion::Australia),
        _ => None,
    }
}

fn crop_percentage(offset: u32) -> Result<u8, EdgeError> {
    u8::try_from(offset).map_err(|_| {
        EdgeError::bad_request("image optimizer crop percentages must be between 0 and 100")
    })
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
    params: &mut ImageOptimizerParams,
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

    params.crop = Some(ImageOptimizerCrop::aspect_ratio(width, height));
}

fn apply_crop_offsets(
    profile_set: &ImageOptimizerProfileSet,
    query: &str,
    params: &mut ImageOptimizerParams,
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
    if crop.focus != ImageOptimizerCropFocus::Default {
        return;
    }

    let x = query_param_value(query, &offset_config.x_param);
    let y = query_param_value(query, &offset_config.y_param);
    if x.is_none() && y.is_none() {
        if offset_config.when_missing == MissingCropOffsetMode::Smart {
            crop.focus = ImageOptimizerCropFocus::Smart;
        }
        return;
    }

    crop.focus = ImageOptimizerCropFocus::Offset {
        x: normalize_offset(x.as_deref(), offset_config),
        y: normalize_offset(y.as_deref(), offset_config),
    };
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
        let &[current, next] = window else {
            continue;
        };
        let midpoint = current + (next.saturating_sub(current) / 2);
        if parsed < midpoint {
            return current;
        }
    }
    config.buckets.last().copied().unwrap_or(first)
}

fn parse_param_string(params: &str) -> Result<ImageOptimizerParams, Report<TrustedServerError>> {
    let mut parsed = ImageOptimizerParams::default();
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

fn parse_format(value: &str) -> Result<OutboundImageFormat, Report<TrustedServerError>> {
    match value.trim().to_ascii_lowercase().as_str() {
        "auto" => Ok(OutboundImageFormat::Auto),
        "avif" => Ok(OutboundImageFormat::Avif),
        "gif" => Ok(OutboundImageFormat::Gif),
        "jpeg" | "jpg" => Ok(OutboundImageFormat::Jpeg),
        "jxl" | "jpegxl" => Ok(OutboundImageFormat::JpegXl),
        "mp4" => Ok(OutboundImageFormat::Mp4),
        "png" => Ok(OutboundImageFormat::Png),
        "webp" => Ok(OutboundImageFormat::WebP),
        _ => Err(Report::new(TrustedServerError::Configuration {
            message: format!("unsupported image optimizer format `{value}`"),
        })),
    }
}

fn parse_resize_filter(value: &str) -> Result<OutboundResizeFilter, Report<TrustedServerError>> {
    match value.trim().to_ascii_lowercase().as_str() {
        "nearest" => Ok(OutboundResizeFilter::Nearest),
        "bilinear" => Ok(OutboundResizeFilter::Bilinear),
        "bicubic" => Ok(OutboundResizeFilter::Bicubic),
        "lanczos2" => Ok(OutboundResizeFilter::Lanczos2),
        "lanczos3" => Ok(OutboundResizeFilter::Lanczos3),
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

fn parse_crop(value: &str) -> Result<ImageOptimizerCrop, Report<TrustedServerError>> {
    let mut parts = value.split(',');
    let ratio = parts.next().unwrap_or_default();
    let (width, height) = parse_crop_ratio(ratio)?;
    let mut smart = false;
    let mut offset_x = None;
    let mut offset_y = None;

    for suffix in parts {
        if suffix == "smart" {
            smart = true;
            continue;
        }
        if let Some(offset) = suffix.strip_prefix("offset-x") {
            offset_x = Some(parse_bounded_u32("crop offset-x", offset, 0, 100)?);
            continue;
        }
        if let Some(offset) = suffix.strip_prefix("offset-y") {
            offset_y = Some(parse_bounded_u32("crop offset-y", offset, 0, 100)?);
            continue;
        }
        return Err(Report::new(TrustedServerError::Configuration {
            message: format!("unsupported image optimizer crop suffix `{suffix}`"),
        }));
    }

    let focus = match (smart, offset_x, offset_y) {
        (false, None, None) => ImageOptimizerCropFocus::Default,
        (true, None, None) => ImageOptimizerCropFocus::Smart,
        (false, Some(x), Some(y)) => ImageOptimizerCropFocus::Offset { x, y },
        (true, _, _) => {
            return Err(Report::new(TrustedServerError::Configuration {
                message: "image optimizer crop cannot combine smart mode with explicit offsets"
                    .to_string(),
            }));
        }
        (false, _, _) => {
            return Err(Report::new(TrustedServerError::Configuration {
                message: "image optimizer crop offsets must include both offset-x and offset-y"
                    .to_string(),
            }));
        }
    };

    Ok(ImageOptimizerCrop {
        width,
        height,
        focus,
    })
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
        AssetImageOptimizerConfig, ImageOptimizerAspectRatioConfig,
        ImageOptimizerCropOffsetsConfig, ImageOptimizerProfileSet, ImageOptimizerSettings,
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

    fn medium_params(query: &str) -> ImageOptimizerParams {
        let set = profile_set();
        let selected = select_profile(&set, Some("medium")).expect("should select profile");
        let mut params = parse_param_string(&set.base_params).expect("should parse base params");
        params.merge_from(
            parse_param_string(set.profiles.get(selected).expect("should get profile"))
                .expect("should parse profile params"),
        );
        apply_aspect_ratio_override(&set, selected, query, &mut params);
        apply_crop_offsets(&set, query, &mut params);
        params
    }

    fn us_east_profile(params: ImageOptimizerParams) -> ImageOptimizerProfile {
        ImageOptimizerProfile {
            region: "us_east".to_string(),
            preserve_origin_query: false,
            params,
        }
    }

    #[test]
    fn profile_for_asset_request_returns_error_when_selected_profile_is_missing() {
        let mut settings = crate::test_support::tests::create_test_settings();
        let mut profile_set = profile_set();
        profile_set.profiles.remove("default");
        settings.image_optimizer = ImageOptimizerSettings {
            profile_sets: std::collections::HashMap::from([(
                "default_images".to_string(),
                profile_set,
            )]),
        };
        let mut route = ProxyAssetRoute::new("/.images/", "https://assets.example.com");
        route.image_optimizer = Some(AssetImageOptimizerConfig {
            enabled: true,
            region: "us_east".to_string(),
            profile_set: "default_images".to_string(),
            origin_query: None,
        });

        let err = profile_for_asset_request(&settings, &route, "")
            .expect_err("should return a configuration error for broken profile sets");

        assert!(
            format!("{err:?}").contains("selected profile `default` is not defined"),
            "should describe the missing selected profile without panicking: {err:?}"
        );
    }

    #[test]
    fn profile_conversion_adds_aspect_ratio_and_smart_crop() {
        let params = medium_params("profile=medium&ar=1-1");

        assert_eq!(
            params,
            ImageOptimizerParams {
                format: Some(OutboundImageFormat::Auto),
                quality: Some(70),
                resize_filter: Some(OutboundResizeFilter::Bicubic),
                width: Some(828),
                height: None,
                crop: Some(ImageOptimizerCrop {
                    width: 1,
                    height: 1,
                    focus: ImageOptimizerCropFocus::Smart,
                }),
            },
            "should merge the profile over the base and add a smart aspect-ratio crop"
        );
    }

    #[test]
    fn profile_conversion_buckets_offsets() {
        let params = medium_params("profile=medium&ar=16-9&x=79&y=bad");

        assert_eq!(
            params.crop,
            Some(ImageOptimizerCrop {
                width: 16,
                height: 9,
                focus: ImageOptimizerCropFocus::Offset { x: 70, y: 50 },
            }),
            "should bucket x and fall back to the default y"
        );
    }

    #[test]
    fn image_optimizer_options_maps_the_profile_to_edgezero_options() {
        let profile = ImageOptimizerProfile {
            preserve_origin_query: true,
            ..us_east_profile(medium_params("profile=medium&ar=16-9&x=79&y=bad"))
        };

        let options = image_optimizer_options(&profile).expect("should map a validated profile");

        assert_eq!(
            options,
            OutboundImageOptimizerOptions::new()
                .region(OutboundImageOptimizerRegion::UsEast)
                .preserve_origin_query(true)
                .format(OutboundImageFormat::Auto)
                .quality(70)
                .expect("should accept quality 70")
                .resize_filter(OutboundResizeFilter::Bicubic)
                .width(828)
                .expect("should accept width 828")
                .crop(16, 9, OutboundCropFocus::Percent { x: 70, y: 50 })
                .expect("should accept the bucketed crop"),
            "should carry every profile parameter, the region, and the query policy"
        );
    }

    #[test]
    fn image_optimizer_options_maps_each_crop_focus() {
        for (focus, expected) in [
            (ImageOptimizerCropFocus::Default, OutboundCropFocus::Default),
            (ImageOptimizerCropFocus::Smart, OutboundCropFocus::Smart),
            (
                ImageOptimizerCropFocus::Offset { x: 0, y: 100 },
                OutboundCropFocus::Percent { x: 0, y: 100 },
            ),
        ] {
            let profile = us_east_profile(ImageOptimizerParams {
                crop: Some(ImageOptimizerCrop {
                    width: 4,
                    height: 3,
                    focus,
                }),
                ..ImageOptimizerParams::default()
            });

            let options = image_optimizer_options(&profile).expect("should map the crop");

            assert_eq!(
                options.crop_setting(),
                Some((4, 3, expected)),
                "should map {focus:?} to {expected:?}"
            );
        }
    }

    #[test]
    fn image_optimizer_options_rejects_values_validation_rules_out() {
        let crop = |x| ImageOptimizerCrop {
            width: 1,
            height: 1,
            focus: ImageOptimizerCropFocus::Offset { x, y: 50 },
        };
        let profiles = [
            ImageOptimizerProfile {
                region: "mars".to_string(),
                ..us_east_profile(ImageOptimizerParams::default())
            },
            us_east_profile(ImageOptimizerParams {
                quality: Some(101),
                ..ImageOptimizerParams::default()
            }),
            us_east_profile(ImageOptimizerParams {
                quality: Some(300),
                ..ImageOptimizerParams::default()
            }),
            us_east_profile(ImageOptimizerParams {
                width: Some(0),
                ..ImageOptimizerParams::default()
            }),
            us_east_profile(ImageOptimizerParams {
                crop: Some(crop(150)),
                ..ImageOptimizerParams::default()
            }),
            us_east_profile(ImageOptimizerParams {
                crop: Some(crop(300)),
                ..ImageOptimizerParams::default()
            }),
        ];

        for profile in profiles {
            let error = image_optimizer_options(&profile)
                .expect_err("should reject a profile that validation rules out");

            assert_eq!(
                error.status(),
                http::StatusCode::BAD_REQUEST,
                "should reject {profile:?} as a bad request"
            );
        }
    }

    #[test]
    fn image_optimizer_region_maps_aliases_to_the_seven_regions() {
        for (aliases, expected) in [
            (
                &["us_east", "us-east-1", "US_EAST"][..],
                OutboundImageOptimizerRegion::UsEast,
            ),
            (
                &["us_central", "us-central-1"][..],
                OutboundImageOptimizerRegion::UsCentral,
            ),
            (
                &["us_west", "us-west-1", "us-west-2"][..],
                OutboundImageOptimizerRegion::UsWest,
            ),
            (
                &["eu_central", "eu-central-1"][..],
                OutboundImageOptimizerRegion::EuCentral,
            ),
            (
                &["eu_west", "eu-west-1"][..],
                OutboundImageOptimizerRegion::EuWest,
            ),
            (&[" asia "][..], OutboundImageOptimizerRegion::Asia),
            (&["Australia"][..], OutboundImageOptimizerRegion::Australia),
        ] {
            for alias in aliases {
                assert_eq!(
                    image_optimizer_region(alias),
                    Some(expected),
                    "should map `{alias}` to {expected:?}"
                );
            }
        }
        for unknown in ["", "mars", "us_east_2", "eu_north_1"] {
            assert_eq!(
                image_optimizer_region(unknown),
                None,
                "should not map `{unknown}`"
            );
        }
    }

    #[test]
    fn parse_crop_rejects_smart_with_offsets_and_lone_offsets() {
        for (value, message) in [
            (
                "1:1,smart,offset-x10,offset-y10",
                "cannot combine smart mode",
            ),
            ("1:1,offset-x10", "must include both offset-x and offset-y"),
            ("1:1,offset-y10", "must include both offset-x and offset-y"),
        ] {
            let error = parse_crop(value).expect_err("should reject the crop");

            assert!(
                format!("{error:?}").contains(message),
                "should reject `{value}` with `{message}`: {error:?}"
            );
        }
    }
}
