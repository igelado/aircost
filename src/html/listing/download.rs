//! Bounded, DNS-pinned downloads for allowlisted listing images.
//!
//! Media discovery establishes the source/host/path trust boundary. This
//! module enforces that boundary at the network layer before any bytes are
//! supplied to Gemini.

use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use reqwest::header::{ACCEPT, CONTENT_TYPE};
use reqwest::redirect::Policy;
use reqwest::Client;
use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::net::lookup_host;
use url::Url;

use super::media::{ListingMediaDiscovery, MediaReference};

pub const MAX_IDENTITY_IMAGE_COUNT: usize = 12;
pub const MAX_IDENTITY_SINGLE_IMAGE_BYTES: usize = 6 * 1024 * 1024;
pub const MAX_IDENTITY_TOTAL_IMAGE_BYTES: usize = 11 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct DownloadedListingImage {
    pub reference: MediaReference,
    pub mime_type: String,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MediaDownloadFailure {
    pub asset_id: String,
    pub message: String,
}

#[derive(Clone, Debug, Default)]
pub struct IdentityImageDownloadReport {
    pub images: Vec<DownloadedListingImage>,
    pub failures: Vec<MediaDownloadFailure>,
    pub selection_truncated: bool,
    pub byte_budget_exhausted: bool,
}

/// Select and download a small, diverse identity-evidence set. Gallery photos
/// and visual service-log scans are interleaved so a long gallery cannot crowd
/// out a labeled registration/serial document.
pub async fn download_identity_images(
    discovery: &ListingMediaDiscovery,
) -> Result<IdentityImageDownloadReport> {
    let (references, selection_truncated) = select_identity_references(discovery);
    if references.is_empty() {
        return Ok(IdentityImageDownloadReport {
            selection_truncated,
            ..IdentityImageDownloadReport::default()
        });
    }

    let host = references[0].media_host.as_str();
    if references.iter().any(|reference| {
        reference.media_host != host
            || reference.fetch_policy.maximum_redirects != 0
            || !reference.fetch_policy.require_public_ip
    }) {
        bail!("identity media references do not share the strict download policy");
    }
    let client = public_dns_pinned_client(host).await?;

    let mut report = IdentityImageDownloadReport {
        selection_truncated,
        ..IdentityImageDownloadReport::default()
    };
    let mut total_bytes = 0usize;
    let mut seen_content_hashes = BTreeSet::new();
    for reference in references {
        let remaining = MAX_IDENTITY_TOTAL_IMAGE_BYTES.saturating_sub(total_bytes);
        if remaining == 0 {
            report.byte_budget_exhausted = true;
            break;
        }
        let maximum_bytes = reference
            .fetch_policy
            .maximum_bytes
            .min(MAX_IDENTITY_SINGLE_IMAGE_BYTES)
            .min(remaining);
        match download_one(&client, reference, maximum_bytes).await {
            Ok(image) => {
                let content_hash: [u8; 32] = Sha256::digest(&image.bytes).into();
                if !seen_content_hashes.insert(content_hash) {
                    report.failures.push(MediaDownloadFailure {
                        asset_id: reference.asset_id.clone(),
                        message: "duplicate image bytes were excluded from visual consensus"
                            .to_string(),
                    });
                    continue;
                }
                total_bytes = total_bytes.saturating_add(image.bytes.len());
                report.images.push(image);
            }
            Err(error) => report.failures.push(MediaDownloadFailure {
                asset_id: reference.asset_id.clone(),
                message: format!("{error:#}"),
            }),
        }
    }
    Ok(report)
}

fn select_identity_references(discovery: &ListingMediaDiscovery) -> (Vec<&MediaReference>, bool) {
    let photos = discovery
        .aircraft_photos
        .iter()
        .filter(|reference| reference.is_visual_image())
        .collect::<Vec<_>>();
    let documents = discovery
        .logbook_attachments
        .iter()
        .filter(|reference| reference.is_visual_image())
        .collect::<Vec<_>>();
    let available = photos.len().saturating_add(documents.len());

    let mut selected = Vec::with_capacity(MAX_IDENTITY_IMAGE_COUNT);
    let mut seen_urls = BTreeSet::new();
    let tiers = [
        (&photos, 0usize, 4usize),
        (&documents, 0usize, 4usize),
        (&photos, 4usize, photos.len()),
        (&documents, 4usize, documents.len()),
    ];
    for (references, start, end) in tiers {
        for reference in references
            .iter()
            .skip(start)
            .take(end.saturating_sub(start))
        {
            if seen_urls.insert(reference.media_url.as_str()) {
                selected.push(*reference);
            }
            if selected.len() == MAX_IDENTITY_IMAGE_COUNT {
                return (selected, available > MAX_IDENTITY_IMAGE_COUNT);
            }
        }
    }
    (selected, available > MAX_IDENTITY_IMAGE_COUNT)
}

async fn public_dns_pinned_client(host: &str) -> Result<Client> {
    let addresses = lookup_host((host, 443))
        .await
        .with_context(|| format!("could not resolve allowlisted media host {host}"))?
        .collect::<BTreeSet<SocketAddr>>();
    if addresses.is_empty() {
        bail!("allowlisted media host {host} resolved to no addresses");
    }
    if let Some(address) = addresses.iter().find(|address| !is_public_ip(address.ip())) {
        bail!(
            "allowlisted media host {host} resolved to non-public address {}",
            address.ip()
        );
    }
    let addresses = addresses.into_iter().collect::<Vec<_>>();
    Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .redirect(Policy::none())
        .resolve_to_addrs(host, &addresses)
        .build()
        .context("could not build DNS-pinned listing media client")
}

async fn download_one(
    client: &Client,
    reference: &MediaReference,
    maximum_bytes: usize,
) -> Result<DownloadedListingImage> {
    if maximum_bytes == 0 {
        bail!("identity image byte budget is exhausted");
    }
    let url = Url::parse(&reference.media_url).context("discovered media URL became invalid")?;
    if url.scheme() != "https"
        || url.host_str() != Some(reference.media_host.as_str())
        || !reference.is_visual_image()
    {
        bail!("discovered media reference changed outside its validated trust boundary");
    }

    let mut response = client
        .get(url)
        .header(
            ACCEPT,
            "image/jpeg,image/png,image/webp,image/heic,image/heif",
        )
        .send()
        .await
        .context("listing media request failed")?;
    if !response.status().is_success() {
        bail!("listing media returned HTTP {}", response.status());
    }
    if response
        .content_length()
        .is_some_and(|length| length > maximum_bytes as u64)
    {
        bail!("listing media exceeds the {maximum_bytes} byte download limit");
    }
    let mime_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(canonical_image_mime)
        .ok_or_else(|| anyhow!("listing media did not return a supported image Content-Type"))?;
    if reference.expected_media_type != "image/*" && reference.expected_media_type != mime_type {
        bail!(
            "listing media Content-Type {mime_type} does not match expected {}",
            reference.expected_media_type
        );
    }

    let mut bytes = Vec::with_capacity(
        response
            .content_length()
            .unwrap_or_default()
            .min(maximum_bytes as u64) as usize,
    );
    while let Some(chunk) = response
        .chunk()
        .await
        .context("listing media response body failed")?
    {
        let next_length = bytes
            .len()
            .checked_add(chunk.len())
            .ok_or_else(|| anyhow!("listing media byte count overflow"))?;
        if next_length > maximum_bytes {
            bail!("listing media exceeds the {maximum_bytes} byte download limit");
        }
        bytes.extend_from_slice(&chunk);
    }
    if bytes.is_empty() || !image_signature_matches(mime_type, &bytes) {
        bail!("listing media body does not match its image Content-Type");
    }

    Ok(DownloadedListingImage {
        reference: reference.clone(),
        mime_type: mime_type.to_string(),
        bytes,
    })
}

fn canonical_image_mime(value: &str) -> Option<&'static str> {
    match value
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "image/jpeg" | "image/jpg" => Some("image/jpeg"),
        "image/png" => Some("image/png"),
        "image/webp" => Some("image/webp"),
        "image/heic" => Some("image/heic"),
        "image/heif" => Some("image/heif"),
        _ => None,
    }
}

fn image_signature_matches(mime_type: &str, bytes: &[u8]) -> bool {
    match mime_type {
        "image/jpeg" => bytes.starts_with(&[0xff, 0xd8, 0xff]),
        "image/png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "image/webp" => bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP",
        "image/heic" | "image/heif" => bytes.len() >= 12 && &bytes[4..8] == b"ftyp",
        _ => false,
    }
}

fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => is_public_ipv4(ip),
        IpAddr::V6(ip) => is_public_ipv6(ip),
    }
}

fn is_public_ipv4(ip: Ipv4Addr) -> bool {
    let [first, second, _, _] = ip.octets();
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_multicast()
        || ip.is_broadcast()
        || ip.is_documentation()
        || first == 0
        || (first == 100 && (64..=127).contains(&second))
        || (first == 192 && second == 0)
        || (first == 198 && (second == 18 || second == 19))
        || first >= 240)
}

fn is_public_ipv6(ip: Ipv6Addr) -> bool {
    if let Some(mapped) = ip.to_ipv4_mapped() {
        return is_public_ipv4(mapped);
    }
    let segments = ip.segments();
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_multicast()
        || (segments[0] & 0xfe00) == 0xfc00
        || (segments[0] & 0xffc0) == 0xfe80
        || (segments[0] == 0x2001 && segments[1] == 0x0db8))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_public_address_ranges() {
        for address in [
            "127.0.0.1",
            "10.0.0.1",
            "100.64.0.1",
            "169.254.10.2",
            "192.0.2.1",
            "198.18.0.1",
            "203.0.113.1",
            "::1",
            "fc00::1",
            "fe80::1",
            "2001:db8::1",
            "::ffff:127.0.0.1",
        ] {
            assert!(!is_public_ip(address.parse().unwrap()), "{address}");
        }
        assert!(is_public_ip("8.8.8.8".parse().unwrap()));
        assert!(is_public_ip("2606:4700:4700::1111".parse().unwrap()));
    }

    #[test]
    fn validates_declared_image_signatures() {
        assert!(image_signature_matches(
            "image/jpeg",
            &[0xff, 0xd8, 0xff, 0xe0]
        ));
        assert!(image_signature_matches(
            "image/png",
            b"\x89PNG\r\n\x1a\nrest"
        ));
        assert!(image_signature_matches(
            "image/webp",
            b"RIFF\x01\x00\x00\x00WEBP"
        ));
        assert!(!image_signature_matches("image/jpeg", b"<html>error"));
    }
}
