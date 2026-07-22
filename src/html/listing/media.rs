//! Deterministic media discovery from retained listing HTML.
//!
//! Discovery never performs a network request. Returned references carry the
//! fetch limits that a downstream resolver must enforce, including public-IP
//! DNS validation. Redirects are disabled because an allowlisted URL is not
//! permission to follow an unvalidated redirect target.

use std::collections::BTreeMap;
use std::fmt;

use html_escape::decode_html_entities;
use scraper::{ElementRef, Html, Selector};
use serde::Serialize;
use url::Url;

pub const MAX_RETAINED_HTML_BYTES: usize = 4_000_000;
pub const MAX_MEDIA_URL_BYTES: usize = 4_096;
pub const MAX_AIRCRAFT_PHOTOS: usize = 24;
// Discovery returns metadata only. Keep enough references to cover Controller's
// multi-image logbook uploads while downstream fetchers enforce byte budgets
// before supplying a much smaller set to a visual model.
pub const MAX_LOGBOOK_ATTACHMENTS: usize = 64;
pub const MAX_AIRCRAFT_PHOTO_BYTES: usize = 12 * 1024 * 1024;
pub const MAX_LOGBOOK_IMAGE_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_LOGBOOK_ATTACHMENT_BYTES: usize = 20 * 1024 * 1024;

const MAX_SOURCE_URL_BYTES: usize = 2_048;
const MAX_LABEL_CHARACTERS: usize = 256;
const CONTROLLER_HOSTS: &[&str] = &["controller.com", "www.controller.com"];
const CONTROLLER_MEDIA_HOST: &str = "media.sandhills.com";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ListingSourceKind {
    Controller,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    AircraftPhoto,
    AirframeLogbook,
    EngineLogbook,
    PropellerLogbook,
    MaintenanceRecord,
    OtherServiceLog,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaLocation {
    GalleryFullscreen,
    GalleryImage,
    OpenGraphImage,
    ServiceLogs,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MediaFetchPolicy {
    pub maximum_bytes: usize,
    pub maximum_redirects: u8,
    /// The downloader must resolve the host itself and reject loopback,
    /// private, link-local, multicast, documentation, and other non-public
    /// addresses before connecting. The same check applies after DNS changes.
    pub require_public_ip: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MediaReference {
    pub listing_source_url: String,
    pub media_url: String,
    pub media_host: String,
    pub asset_id: String,
    pub kind: MediaKind,
    pub location: MediaLocation,
    pub label: Option<String>,
    pub section_label: Option<String>,
    pub is_original: bool,
    pub requested_width_px: Option<u32>,
    pub requested_height_px: Option<u32>,
    pub expected_media_type: String,
    pub priority: usize,
    pub fetch_policy: MediaFetchPolicy,
}

impl MediaReference {
    pub fn is_visual_image(&self) -> bool {
        self.expected_media_type.starts_with("image/")
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ListingMediaDiscovery {
    pub listing_source_url: String,
    pub source_kind: ListingSourceKind,
    pub aircraft_photos: Vec<MediaReference>,
    pub logbook_attachments: Vec<MediaReference>,
    pub rejected_reference_count: usize,
    pub photos_truncated: bool,
    pub attachments_truncated: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MediaDiscoveryError {
    SourceUrlTooLong {
        maximum_bytes: usize,
    },
    InvalidSourceUrl,
    UnsafeSourceUrl,
    UnsupportedSourceHost,
    UnsupportedSourcePath,
    RetainedHtmlTooLarge {
        actual_bytes: usize,
        maximum_bytes: usize,
    },
}

impl fmt::Display for MediaDiscoveryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SourceUrlTooLong { maximum_bytes } => {
                write!(
                    formatter,
                    "listing source URL exceeds {maximum_bytes} bytes"
                )
            }
            Self::InvalidSourceUrl => write!(formatter, "listing source URL is invalid"),
            Self::UnsafeSourceUrl => write!(formatter, "listing source URL is not safe HTTPS"),
            Self::UnsupportedSourceHost => {
                write!(formatter, "listing source host is not allowlisted")
            }
            Self::UnsupportedSourcePath => {
                write!(
                    formatter,
                    "listing source URL is not a Controller sale listing"
                )
            }
            Self::RetainedHtmlTooLarge {
                actual_bytes,
                maximum_bytes,
            } => write!(
                formatter,
                "retained listing HTML is {actual_bytes} bytes; maximum is {maximum_bytes}"
            ),
        }
    }
}

impl std::error::Error for MediaDiscoveryError {}

#[derive(Clone, Debug)]
struct SafeAsset {
    url: String,
    host: String,
    asset_id: String,
    width: Option<u32>,
    height: Option<u32>,
    is_original: bool,
    expected_media_type: &'static str,
    maximum_bytes: usize,
}

#[derive(Clone, Debug)]
struct Candidate {
    reference: MediaReference,
    first_order: usize,
    quality: CandidateQuality,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct CandidateQuality {
    is_original: bool,
    source_rank: u16,
    image_area: u64,
}

/// Discover original aircraft photos and signed service-log attachments from one
/// retained Controller listing page. It accepts only a fixed source-to-media
/// trust mapping and never honors HTML-provided base URLs or arbitrary hosts.
pub fn discover(
    source_url: &str,
    retained_html: &str,
) -> Result<ListingMediaDiscovery, MediaDiscoveryError> {
    let source_url = validate_source_url(source_url)?;
    if retained_html.len() > MAX_RETAINED_HTML_BYTES {
        return Err(MediaDiscoveryError::RetainedHtmlTooLarge {
            actual_bytes: retained_html.len(),
            maximum_bytes: MAX_RETAINED_HTML_BYTES,
        });
    }

    let document = Html::parse_document(retained_html);
    let mut rejected_reference_count = 0usize;
    let mut photos = BTreeMap::<String, Candidate>::new();
    discover_controller_photos(
        &document,
        &source_url,
        &mut photos,
        &mut rejected_reference_count,
    );
    let mut attachments = BTreeMap::<String, Candidate>::new();
    discover_controller_service_logs(
        &document,
        &source_url,
        &mut attachments,
        &mut rejected_reference_count,
    );

    let (aircraft_photos, photos_truncated) =
        finalize_candidates(photos, MAX_AIRCRAFT_PHOTOS, |candidate| {
            (!candidate.reference.is_original, candidate.first_order)
        });
    let (logbook_attachments, attachments_truncated) =
        finalize_candidates(attachments, MAX_LOGBOOK_ATTACHMENTS, |candidate| {
            (
                attachment_priority(candidate.reference.kind),
                candidate.first_order,
            )
        });

    Ok(ListingMediaDiscovery {
        listing_source_url: source_url,
        source_kind: ListingSourceKind::Controller,
        aircraft_photos,
        logbook_attachments,
        rejected_reference_count,
        photos_truncated,
        attachments_truncated,
    })
}

fn validate_source_url(source_url: &str) -> Result<String, MediaDiscoveryError> {
    let source_url = source_url.trim();
    if source_url.len() > MAX_SOURCE_URL_BYTES {
        return Err(MediaDiscoveryError::SourceUrlTooLong {
            maximum_bytes: MAX_SOURCE_URL_BYTES,
        });
    }
    let url = Url::parse(source_url).map_err(|_| MediaDiscoveryError::InvalidSourceUrl)?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some_and(|port| port != 443)
        || url.fragment().is_some()
    {
        return Err(MediaDiscoveryError::UnsafeSourceUrl);
    }
    let host = url
        .host_str()
        .ok_or(MediaDiscoveryError::InvalidSourceUrl)?
        .to_ascii_lowercase();
    if !CONTROLLER_HOSTS.contains(&host.as_str()) {
        return Err(MediaDiscoveryError::UnsupportedSourceHost);
    }
    let segments = url
        .path_segments()
        .ok_or(MediaDiscoveryError::UnsupportedSourcePath)?
        .collect::<Vec<_>>();
    if segments.len() < 4
        || segments[0] != "listing"
        || segments[1] != "for-sale"
        || segments[2].is_empty()
        || !segments[2].bytes().all(|byte| byte.is_ascii_digit())
        || segments[3].is_empty()
    {
        return Err(MediaDiscoveryError::UnsupportedSourcePath);
    }
    Ok(source_url.to_string())
}

fn discover_controller_photos(
    document: &Html,
    source_url: &str,
    candidates: &mut BTreeMap<String, Candidate>,
    rejected: &mut usize,
) {
    let gallery_selector = Selector::parse(".mc-items img").expect("static selector");
    let attributes = [
        ("data-fullscreen", MediaLocation::GalleryFullscreen, 300_u16),
        ("data-src", MediaLocation::GalleryImage, 200_u16),
        ("src", MediaLocation::GalleryImage, 200_u16),
    ];
    let mut order = 0usize;
    for image in document.select(&gallery_selector) {
        for (attribute, location, base_quality) in attributes {
            let Some(raw_url) = image.value().attr(attribute) else {
                continue;
            };
            let Some(asset) = accepted_sandhills_asset(raw_url, AssetKind::Image) else {
                *rejected = rejected.saturating_add(1);
                continue;
            };
            let quality = CandidateQuality {
                is_original: asset.is_original,
                source_rank: base_quality,
                image_area: image_area_score(asset.width, asset.height),
            };
            let reference = media_reference(
                source_url,
                asset.clone(),
                MediaKind::AircraftPhoto,
                location,
                usable_label(image.value().attr("alt")),
                None,
            );
            insert_candidate(candidates, asset.asset_id, reference, order, quality);
            order = order.saturating_add(1);
        }
    }

    let open_graph_selector =
        Selector::parse(r#"meta[property="og:image"]"#).expect("static selector");
    for meta in document.select(&open_graph_selector) {
        let Some(raw_url) = meta.value().attr("content") else {
            continue;
        };
        let Some(asset) = accepted_sandhills_asset(raw_url, AssetKind::Image) else {
            *rejected = rejected.saturating_add(1);
            continue;
        };
        let quality = CandidateQuality {
            is_original: asset.is_original,
            source_rank: 100,
            image_area: image_area_score(asset.width, asset.height),
        };
        let reference = media_reference(
            source_url,
            asset.clone(),
            MediaKind::AircraftPhoto,
            MediaLocation::OpenGraphImage,
            None,
            None,
        );
        insert_candidate(candidates, asset.asset_id, reference, order, quality);
        order = order.saturating_add(1);
    }
}

fn discover_controller_service_logs(
    document: &Html,
    source_url: &str,
    candidates: &mut BTreeMap<String, Candidate>,
    rejected: &mut usize,
) {
    let wrapper_selector = Selector::parse(".detail__specs-service-logs .detail__specs-wrapper")
        .expect("static selector");
    let label_selector = Selector::parse(".detail__specs-label").expect("static selector");
    let link_selector = Selector::parse("a[href]").expect("static selector");
    let mut order = 0usize;
    for wrapper in document.select(&wrapper_selector) {
        let section_label = wrapper
            .select(&label_selector)
            .next()
            .and_then(element_text);
        for link in wrapper.select(&link_selector) {
            let Some(raw_url) = link.value().attr("href") else {
                continue;
            };
            let Some(asset) = accepted_sandhills_asset(raw_url, AssetKind::ServiceLog) else {
                *rejected = rejected.saturating_add(1);
                continue;
            };
            let label =
                element_text(link).or_else(|| usable_label(link.value().attr("aria-label")));
            let kind = classify_attachment(section_label.as_deref(), label.as_deref());
            let quality = CandidateQuality {
                is_original: true,
                source_rank: 1_000_u16.saturating_sub(u16::from(attachment_priority(kind)) * 100),
                image_area: 0,
            };
            let reference = media_reference(
                source_url,
                asset.clone(),
                kind,
                MediaLocation::ServiceLogs,
                label,
                section_label.clone(),
            );
            insert_candidate(candidates, asset.asset_id, reference, order, quality);
            order = order.saturating_add(1);
        }
    }
}

fn media_reference(
    source_url: &str,
    asset: SafeAsset,
    kind: MediaKind,
    location: MediaLocation,
    label: Option<String>,
    section_label: Option<String>,
) -> MediaReference {
    let expected_media_type = asset.expected_media_type;
    let maximum_bytes = asset.maximum_bytes;
    MediaReference {
        listing_source_url: source_url.to_string(),
        media_url: asset.url,
        media_host: asset.host,
        asset_id: asset.asset_id,
        kind,
        location,
        label,
        section_label,
        is_original: asset.is_original,
        requested_width_px: asset.width,
        requested_height_px: asset.height,
        expected_media_type: expected_media_type.to_string(),
        priority: 0,
        fetch_policy: MediaFetchPolicy {
            maximum_bytes,
            maximum_redirects: 0,
            require_public_ip: true,
        },
    }
}

fn insert_candidate(
    candidates: &mut BTreeMap<String, Candidate>,
    asset_id: String,
    reference: MediaReference,
    order: usize,
    quality: CandidateQuality,
) {
    match candidates.get_mut(&asset_id) {
        Some(existing) => {
            existing.first_order = existing.first_order.min(order);
            if quality > existing.quality {
                existing.reference = reference;
                existing.quality = quality;
            }
        }
        None => {
            candidates.insert(
                asset_id,
                Candidate {
                    reference,
                    first_order: order,
                    quality,
                },
            );
        }
    }
}

fn finalize_candidates<K: Ord>(
    candidates: BTreeMap<String, Candidate>,
    maximum: usize,
    sort_key: impl Fn(&Candidate) -> K,
) -> (Vec<MediaReference>, bool) {
    let mut candidates = candidates.into_values().collect::<Vec<_>>();
    candidates.sort_by_key(sort_key);
    let truncated = candidates.len() > maximum;
    candidates.truncate(maximum);
    let references = candidates
        .into_iter()
        .enumerate()
        .map(|(index, mut candidate)| {
            candidate.reference.priority = index + 1;
            candidate.reference
        })
        .collect();
    (references, truncated)
}

#[derive(Clone, Copy)]
enum AssetKind {
    Image,
    ServiceLog,
}

fn accepted_sandhills_asset(raw_url: &str, kind: AssetKind) -> Option<SafeAsset> {
    let raw_url = decode_html_entities(raw_url.trim()).into_owned();
    if raw_url.is_empty() || raw_url.len() > MAX_MEDIA_URL_BYTES {
        return None;
    }
    let url = Url::parse(&raw_url).ok()?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some_and(|port| port != 443)
        || url.fragment().is_some()
    {
        return None;
    }
    let host = url.host_str()?.to_ascii_lowercase();
    if host != CONTROLLER_MEDIA_HOST {
        return None;
    }
    let expected_path = match kind {
        AssetKind::Image => "/img.axd",
        AssetKind::ServiceLog => "/doc.axd",
    };
    if !url.path().eq_ignore_ascii_case(expected_path) {
        return None;
    }
    let query = url
        .query_pairs()
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect::<Vec<_>>();
    let asset_id = unique_query_value(&query, "id")?;
    if asset_id.is_empty()
        || asset_id.len() > 20
        || !asset_id.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let checksum = unique_query_value(&query, "checksum")?;
    if checksum.is_empty()
        || checksum.len() > 512
        || !checksum.is_ascii()
        || checksum.chars().any(char::is_control)
    {
        return None;
    }

    let (width, height, is_original, expected_media_type, maximum_bytes) = match kind {
        AssetKind::Image => {
            let width = optional_dimension(&query, "w")?;
            let height = optional_dimension(&query, "h")?;
            let size = optional_unique_query_value(&query, "sz")?;
            let is_original = width == Some(0)
                && height == Some(0)
                && size
                    .as_deref()
                    .is_some_and(|size| size.eq_ignore_ascii_case("max"));
            (
                width,
                height,
                is_original,
                "image/*",
                MAX_AIRCRAFT_PHOTO_BYTES,
            )
        }
        AssetKind::ServiceLog => {
            let extension = unique_query_value(&query, "ext")?;
            let (expected_media_type, maximum_bytes) = if extension.eq_ignore_ascii_case(".pdf") {
                ("application/pdf", MAX_LOGBOOK_ATTACHMENT_BYTES)
            } else if extension.eq_ignore_ascii_case(".jpeg")
                || extension.eq_ignore_ascii_case(".jpg")
            {
                ("image/jpeg", MAX_LOGBOOK_IMAGE_BYTES)
            } else if extension.eq_ignore_ascii_case(".png") {
                ("image/png", MAX_LOGBOOK_IMAGE_BYTES)
            } else {
                return None;
            };
            (None, None, true, expected_media_type, maximum_bytes)
        }
    };
    Some(SafeAsset {
        url: url.to_string(),
        host,
        asset_id,
        width,
        height,
        is_original,
        expected_media_type,
        maximum_bytes,
    })
}

fn unique_query_value(query: &[(String, String)], name: &str) -> Option<String> {
    let mut values = query
        .iter()
        .filter(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.clone());
    let value = values.next()?;
    values.next().is_none().then_some(value)
}

fn optional_unique_query_value(query: &[(String, String)], name: &str) -> Option<Option<String>> {
    let mut values = query
        .iter()
        .filter(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.clone());
    let value = values.next();
    values.next().is_none().then_some(value)
}

fn optional_dimension(query: &[(String, String)], name: &str) -> Option<Option<u32>> {
    optional_unique_query_value(query, name)?.map_or(Some(None), |value| {
        value
            .parse::<u32>()
            .ok()
            .filter(|dimension| *dimension <= 16_384)
            .map(Some)
    })
}

fn image_area_score(width: Option<u32>, height: Option<u32>) -> u64 {
    u64::from(width.unwrap_or_default()).saturating_mul(u64::from(height.unwrap_or_default()))
}

fn classify_attachment(section: Option<&str>, label: Option<&str>) -> MediaKind {
    let context = format!(
        "{} {}",
        section.unwrap_or_default(),
        label.unwrap_or_default()
    )
    .to_ascii_lowercase();
    if context.contains("prop") {
        MediaKind::PropellerLogbook
    } else if context.contains("engine") {
        MediaKind::EngineLogbook
    } else if context.contains("airframe") || context.contains("aircraft") {
        MediaKind::AirframeLogbook
    } else if [
        "logbook",
        "maintenance",
        "inspection",
        "service",
        "form 337",
        "337",
    ]
    .iter()
    .any(|keyword| context.contains(keyword))
    {
        MediaKind::MaintenanceRecord
    } else {
        MediaKind::OtherServiceLog
    }
}

fn attachment_priority(kind: MediaKind) -> u8 {
    match kind {
        MediaKind::AirframeLogbook => 0,
        MediaKind::EngineLogbook => 1,
        MediaKind::PropellerLogbook => 2,
        MediaKind::MaintenanceRecord => 3,
        MediaKind::OtherServiceLog => 4,
        MediaKind::AircraftPhoto => 5,
    }
}

fn element_text(element: ElementRef<'_>) -> Option<String> {
    usable_label(Some(&element.text().collect::<Vec<_>>().join(" ")))
}

fn usable_label(value: Option<&str>) -> Option<String> {
    let value = decode_html_entities(value?.trim())
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if value.is_empty() {
        return None;
    }
    Some(value.chars().take(MAX_LABEL_CHARACTERS).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str = "https://www.controller.com/listing/for-sale/256858675/2010-cessna-turbo-182t-skylane-piston-single-aircraft";

    fn image_url(id: u64, width: u32, height: u32) -> String {
        format!(
            "https://media.sandhills.com/img.axd?id={id}&wid=4326165471&w={width}&h={height}&sz=Max&checksum=SIGNED{id}"
        )
    }

    fn document_url(id: u64) -> String {
        format!("https://media.sandhills.com/doc.axd?id={id}&ext=.pdf&dl=False&checksum=SIGNED{id}")
    }

    #[test]
    fn controller_gallery_prefers_original_fullscreen_assets_and_preserves_order() {
        let scaled_one = image_url(11026177352, 614, 460);
        let original_one = image_url(11026177352, 0, 0);
        let scaled_two = image_url(11026177353, 614, 460);
        let original_two = image_url(11026177353, 0, 0);
        let html = format!(
            r#"
            <meta property="og:image" content="{scaled_one}">
            <div class="mc-items">
              <div class="mc-item mc-img mc-selected">
                <img src="{scaled_one}" alt="2010 Cessna Turbo 182T exterior">
                <img data-fullscreen="{original_one}" alt="2010 Cessna Turbo 182T exterior">
              </div>
              <div class="mc-item mc-img">
                <img data-src="{scaled_two}" alt="2010 Cessna Turbo 182T panel">
                <img data-fullscreen="{original_two}" alt="2010 Cessna Turbo 182T panel">
              </div>
            </div>
            "#
        );

        let discovery = discover(SOURCE, &html).unwrap();

        assert_eq!(discovery.aircraft_photos.len(), 2);
        assert_eq!(discovery.aircraft_photos[0].asset_id, "11026177352");
        assert_eq!(discovery.aircraft_photos[1].asset_id, "11026177353");
        assert!(discovery
            .aircraft_photos
            .iter()
            .all(|photo| photo.is_original));
        assert!(discovery
            .aircraft_photos
            .iter()
            .all(|photo| photo.location == MediaLocation::GalleryFullscreen));
        assert_eq!(discovery.aircraft_photos[0].requested_width_px, Some(0));
        assert_eq!(discovery.aircraft_photos[0].listing_source_url, SOURCE);
        assert_eq!(
            discovery.aircraft_photos[0].fetch_policy.maximum_bytes,
            MAX_AIRCRAFT_PHOTO_BYTES
        );
        assert_eq!(
            discovery.aircraft_photos[0].fetch_policy.maximum_redirects,
            0
        );
        assert!(discovery.aircraft_photos[0].fetch_policy.require_public_ip);
    }

    #[test]
    fn controller_service_logs_are_classified_and_prioritized() {
        let engine = document_url(11025781435);
        let airframe = document_url(11025781431);
        let propeller = document_url(11025781436);
        let miscellaneous = document_url(11025781437);
        let html = format!(
            r#"
            <div class="detail__specs-service-logs">
              <h3>Service Logs</h3>
              <div class="detail__specs-wrapper">
                <div class="detail__specs-label">Engine</div>
                <div class="detail__specs-value"><a href="{engine}">N478GP Engine Log.pdf</a></div>
              </div>
              <div class="detail__specs-wrapper">
                <div class="detail__specs-label">Aircraft</div>
                <div class="detail__specs-value"><a href="{airframe}">N478GP Airframe Maintenance.pdf</a></div>
              </div>
              <div class="detail__specs-wrapper">
                <div class="detail__specs-label">Propeller</div>
                <div class="detail__specs-value"><a href="{propeller}">Prop Logbook.pdf</a></div>
              </div>
              <div class="detail__specs-wrapper">
                <div class="detail__specs-label">Miscellaneous</div>
                <div class="detail__specs-value"><a href="{miscellaneous}">Weight and Balance.pdf</a></div>
              </div>
            </div>
            "#
        );

        let discovery = discover(SOURCE, &html).unwrap();

        assert_eq!(discovery.logbook_attachments.len(), 4);
        assert_eq!(
            discovery.logbook_attachments[0].kind,
            MediaKind::AirframeLogbook
        );
        assert_eq!(
            discovery.logbook_attachments[1].kind,
            MediaKind::EngineLogbook
        );
        assert_eq!(
            discovery.logbook_attachments[2].kind,
            MediaKind::PropellerLogbook
        );
        assert_eq!(
            discovery.logbook_attachments[3].kind,
            MediaKind::OtherServiceLog
        );
        assert!(discovery
            .logbook_attachments
            .iter()
            .all(|attachment| attachment.expected_media_type == "application/pdf"));
    }

    #[test]
    fn controller_doc_endpoint_images_are_visual_inputs_with_exact_mime_type() {
        // Shape retained in plugin submission 20. Controller serves these
        // original logbook/data-plate photos from doc.axd rather than img.axd.
        let source = "https://www.controller.com/listing/for-sale/252742967/1965-cessna-182-skylane-piston-single-aircraft";
        let html = r#"
            <div class="detail__specs-service-logs">
              <div class="detail__specs-wrapper">
                <div class="detail__specs-label">Miscellaneous</div>
                <div class="detail__specs-value">
                  <a class="detail__specs-link" target="_blank" content="noindex"
                     href="https://media.sandhills.com/doc.axd?id=11002241579&amp;p=&amp;ext=.jpeg&amp;dl=False&amp;wt=False&amp;checksum=nJi2GsLJLuX43M%2f%2fd3v%2bqSKRJMb75jm6OH%2fRu1x9RwiDfTZRK%2fG23rE3T9KCEZvsDkg6fOXB27E%3d"
                     aria-label="IMG_5619.jpeg (Opens in a new tab)">IMG_5619.jpeg</a>
                  <a class="detail__specs-link" target="_blank" content="noindex"
                     href="https://media.sandhills.com/doc.axd?id=11002241220&amp;p=&amp;ext=.jpeg&amp;dl=False&amp;wt=False&amp;checksum=nJi2GsLJLuVlD99LZm2Re2sMd5vZRV9X44RqyBtATr4JCtvdVNj4kw2Jw6Owk2fkb2GPbD1QdMw%3d"
                     aria-label="IMG_5450.jpeg (Opens in a new tab)">IMG_5450.jpeg</a>
                </div>
              </div>
            </div>
        "#;

        let discovery = discover(source, html).unwrap();

        assert_eq!(discovery.logbook_attachments.len(), 2);
        assert_eq!(discovery.logbook_attachments[0].asset_id, "11002241579");
        assert_eq!(discovery.logbook_attachments[1].asset_id, "11002241220");
        assert!(discovery
            .logbook_attachments
            .iter()
            .all(MediaReference::is_visual_image));
        assert!(discovery.logbook_attachments.iter().all(|attachment| {
            attachment.expected_media_type == "image/jpeg"
                && attachment.fetch_policy.maximum_bytes == MAX_LOGBOOK_IMAGE_BYTES
        }));
    }

    #[test]
    fn source_and_media_allowlists_fail_closed_against_ssrf_shapes() {
        for unsafe_source in [
            "http://www.controller.com/listing/for-sale/1/test",
            "https://controller.com.evil.test/listing/for-sale/1/test",
            "https://user@www.controller.com/listing/for-sale/1/test",
            "https://www.controller.com/search",
        ] {
            assert!(discover(unsafe_source, "").is_err(), "{unsafe_source}");
        }

        let valid = image_url(1, 0, 0);
        let html = format!(
            r#"
            <base href="https://media.sandhills.com/">
            <div class="mc-items">
              <img data-fullscreen="{valid}">
              <img data-fullscreen="http://media.sandhills.com/img.axd?id=2&amp;w=0&amp;h=0&amp;sz=Max&amp;checksum=x">
              <img data-fullscreen="https://media.sandhills.com.evil.test/img.axd?id=3&amp;w=0&amp;h=0&amp;sz=Max&amp;checksum=x">
              <img data-fullscreen="https://media.sandhills.com@127.0.0.1/img.axd?id=4&amp;w=0&amp;h=0&amp;sz=Max&amp;checksum=x">
              <img data-fullscreen="//media.sandhills.com/img.axd?id=5&amp;w=0&amp;h=0&amp;sz=Max&amp;checksum=x">
              <img data-fullscreen="img.axd?id=6&amp;w=0&amp;h=0&amp;sz=Max&amp;checksum=x">
              <img data-fullscreen="javascript:alert(1)">
            </div>
            "#
        );

        let discovery = discover(SOURCE, &html).unwrap();

        assert_eq!(discovery.aircraft_photos.len(), 1);
        assert_eq!(discovery.aircraft_photos[0].asset_id, "1");
        assert_eq!(discovery.rejected_reference_count, 6);
    }

    #[test]
    fn unrelated_page_images_and_unsigned_assets_are_not_discovered() {
        let html = format!(
            r#"
            <img src="{}" alt="recommended listing">
            <meta property="og:image" content="https://media.sandhills.com/CDN/Images/app-icon.png">
            <div class="mc-items">
              <img src="https://media.sandhills.com/img.axd?id=12&amp;w=0&amp;h=0&amp;sz=Max">
            </div>
            <div class="detail__specs-service-logs">
              <div class="detail__specs-wrapper">
                <div class="detail__specs-label">Aircraft</div>
                <a href="https://media.sandhills.com/doc.axd?id=13&amp;ext=.exe&amp;checksum=x">unsafe</a>
              </div>
            </div>
            "#,
            image_url(11, 0, 0)
        );

        let discovery = discover(SOURCE, &html).unwrap();

        assert!(discovery.aircraft_photos.is_empty());
        assert!(discovery.logbook_attachments.is_empty());
        assert_eq!(discovery.rejected_reference_count, 3);
    }

    #[test]
    fn media_counts_and_retained_html_size_are_bounded() {
        let mut html = String::from("<div class=\"mc-items\">");
        for id in 1..=(MAX_AIRCRAFT_PHOTOS + 5) {
            html.push_str(&format!(
                "<img data-fullscreen=\"{}\">",
                image_url(id as u64, 0, 0)
            ));
        }
        html.push_str("</div>");

        let discovery = discover(SOURCE, &html).unwrap();
        assert_eq!(discovery.aircraft_photos.len(), MAX_AIRCRAFT_PHOTOS);
        assert!(discovery.photos_truncated);

        let oversized = "x".repeat(MAX_RETAINED_HTML_BYTES + 1);
        assert!(matches!(
            discover(SOURCE, &oversized),
            Err(MediaDiscoveryError::RetainedHtmlTooLarge { .. })
        ));
    }
}
