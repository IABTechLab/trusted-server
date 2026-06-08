//! Platform-neutral Image Optimizer request metadata.
//!
//! These DTOs describe the closed image transformation set that Trusted Server
//! attaches to an outbound asset proxy request. They intentionally avoid Fastly
//! SDK types so the profile-table logic can stay in core while the Fastly
//! adapter remains responsible for translating metadata into
//! `fastly::image_optimizer::ImageOptimizerOptions`.
//!
//! Unsupported adapters should reject requests carrying this metadata rather
//! than silently dropping transformations.

/// Platform-neutral Image Optimizer processing region.
///
/// These variants mirror the regions currently exposed by the Fastly SDK while
/// keeping config validation in core independent from adapter-specific types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatformImageOptimizerRegion {
    /// Apply image optimization in the eastern United States.
    UsEast,
    /// Apply image optimization in the central United States.
    UsCentral,
    /// Apply image optimization in the western United States.
    UsWest,
    /// Apply image optimization in central Europe.
    EuCentral,
    /// Apply image optimization in western Europe.
    EuWest,
    /// Apply image optimization in Asia.
    Asia,
    /// Apply image optimization in Australia.
    Australia,
}

impl PlatformImageOptimizerRegion {
    /// Parse a configured region string into a supported Image Optimizer region.
    #[must_use]
    pub fn parse(region: &str) -> Option<Self> {
        match region
            .trim()
            .to_ascii_lowercase()
            .replace('-', "_")
            .as_str()
        {
            "us_east" | "us_east_1" => Some(Self::UsEast),
            "us_central" | "us_central_1" => Some(Self::UsCentral),
            "us_west" | "us_west_1" | "us_west_2" => Some(Self::UsWest),
            "eu_central" | "eu_central_1" => Some(Self::EuCentral),
            "eu_west" | "eu_west_1" => Some(Self::EuWest),
            "asia" => Some(Self::Asia),
            "australia" => Some(Self::Australia),
            _ => None,
        }
    }
}

/// Platform-neutral Image Optimizer options for an upstream request.
///
/// Core code stores only a closed transformation set here. The Fastly adapter is
/// responsible for translating these values to SDK-specific
/// `ImageOptimizerOptions`, while adapters without Image Optimizer support
/// should reject requests carrying this metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformImageOptimizerOptions {
    /// Image Optimizer processing region understood by the target adapter.
    pub region: String,
    /// Whether non-IO query parameters should be preserved on the origin request.
    pub preserve_query_string_on_origin_request: bool,
    /// Transformation parameters to apply.
    pub params: PlatformImageOptimizerParams,
}

impl PlatformImageOptimizerOptions {
    /// Create Image Optimizer options for the given region and params.
    #[must_use]
    pub fn new(region: impl Into<String>, params: PlatformImageOptimizerParams) -> Self {
        Self {
            region: region.into(),
            preserve_query_string_on_origin_request: false,
            params,
        }
    }

    /// Preserve non-IO query parameters on the origin request.
    ///
    /// Asset routes with profile-table IO reject arbitrary query preservation by
    /// default because client query parameters can otherwise become additional
    /// Image Optimizer inputs.
    #[must_use]
    pub fn with_preserve_query_string_on_origin_request(mut self, preserve: bool) -> Self {
        self.preserve_query_string_on_origin_request = preserve;
        self
    }
}

/// Platform-neutral subset of image transformation parameters.
///
/// This intentionally mirrors only the parameters accepted by asset-route
/// profile tables: format, quality, resize filter, dimensions, and crop. Client
/// query strings are converted into this closed set before the request reaches a
/// platform adapter.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PlatformImageOptimizerParams {
    /// Output format such as `auto` or `webp`.
    pub format: Option<String>,
    /// Output quality from 0 to 100.
    pub quality: Option<u32>,
    /// Resize filter such as `bicubic`.
    pub resize_filter: Option<String>,
    /// Output width in pixels.
    pub width: Option<u32>,
    /// Output height in pixels.
    pub height: Option<u32>,
    /// Crop transformation.
    pub crop: Option<PlatformImageOptimizerCrop>,
}

impl PlatformImageOptimizerParams {
    /// Merge another param set into this one, with `other` taking precedence.
    pub fn merge_from(&mut self, other: Self) {
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

/// Platform-neutral crop transformation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformImageOptimizerCrop {
    /// Aspect-ratio width component.
    pub width: u32,
    /// Aspect-ratio height component.
    pub height: u32,
    /// Optional crop focus mode.
    pub mode: Option<PlatformImageOptimizerCropMode>,
    /// Optional x-axis crop offset bucket.
    pub offset_x: Option<u32>,
    /// Optional y-axis crop offset bucket.
    pub offset_y: Option<u32>,
}

impl PlatformImageOptimizerCrop {
    /// Create a bare aspect-ratio crop.
    #[must_use]
    pub fn aspect_ratio(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            mode: None,
            offset_x: None,
            offset_y: None,
        }
    }

    /// Returns true when no focus mode or explicit offsets have been applied.
    #[must_use]
    pub fn is_bare_aspect_ratio(&self) -> bool {
        self.mode.is_none() && self.offset_x.is_none() && self.offset_y.is_none()
    }
}

/// Platform-neutral crop focus mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatformImageOptimizerCropMode {
    /// Use Fastly IO smart crop mode.
    Smart,
}
