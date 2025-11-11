use std::fmt::Write;
use std::fs::File;
use std::io::{BufReader, Seek, SeekFrom};
use std::path::Path;

use anyhow::{Context, Result};
use exif::{Error as ExifError, Exif, Field, In, Reader as ExifReader, Tag, Value};
use http_range_client::HttpReader;
use log::warn;
use reqwest::blocking::Client;
use serde_json::Value as JsonValue;

/// Downloads the image from the given URL and returns a textual summary of the
/// leading bytes and EXIF metadata.
pub fn summarize_exif(url: &str) -> Result<String> {
    let mut reader = HttpReader::new(url);
    reader.set_min_req_size(500 * 1024);

    reader
        .seek(SeekFrom::Start(0))
        .context("Failed to seek to start of HTTP stream")?;

    let mut buf_reader = BufReader::new(reader);
    let exif_reader = ExifReader::new();

    let exif = match exif_reader.read_from_container(&mut buf_reader) {
        Ok(exif) => exif,
        Err(ExifError::NotFound(_)) => return Ok(build_empty_caption()),
        Err(err) => return Err(err.into()),
    };

    let summary = ParsedExif::from_exif(&exif);
    Ok(build_caption(&summary))
}

/// Reads EXIF data from a local file and returns the formatted summary.
pub fn summarize_exif_from_file(path: &Path) -> Result<String> {
    let file = File::open(path)
        .with_context(|| format!("Failed to open local image at `{}`", path.display()))?;
    let mut buf_reader = BufReader::new(file);
    let exif_reader = ExifReader::new();

    let exif = match exif_reader.read_from_container(&mut buf_reader) {
        Ok(exif) => exif,
        Err(ExifError::NotFound(_)) => return Ok(build_empty_caption()),
        Err(err) => return Err(err.into()),
    };

    let summary = ParsedExif::from_exif(&exif);
    Ok(build_caption(&summary))
}

struct ParsedExif {
    title: Option<String>,
    camera: String,
    lens: String,
    focal_length: Option<String>,
    focal_length_val: Option<f64>,
    focal_length_35mm: Option<String>,
    focal_length_35mm_val: Option<f64>,
    aperture: Option<String>,
    shutter: Option<String>,
    iso: Option<String>,
    datetime: Option<String>,
    location: Option<String>,
    country: Option<String>,
    gps: Option<String>,
}

struct GpsData {
    display: String,
    latitude: f64,
    longitude: f64,
}

const NOMINATIM_ENDPOINT: &str = "https://nominatim.openstreetmap.org/reverse";
const NOMINATIM_USER_AGENT: &str = "fotobot_rs/0.1 (https://github.com/user/fotobot_rs)";

impl ParsedExif {
    fn from_exif(exif: &Exif) -> Self {
        let title = first_string(exif, &[Tag::ImageDescription]);

        let make = first_string(exif, &[Tag::Make]);
        let model = first_string(exif, &[Tag::Model]);
        let camera = match (make, model.clone()) {
            (Some(make), Some(model)) => format!("{make} {model}"),
            (Some(make), None) => make,
            (None, Some(model)) => model,
            (None, None) => String::from("Unknown Camera"),
        };

        let lens_model = first_string(exif, &[Tag::LensModel]);
        let lens_spec = lens_specification(exif);
        let lens = lens_model
            .or(lens_spec)
            .unwrap_or_else(|| String::from("Unknown Lens"));

        let (focal_length, focal_length_val) = focal_length_values(exif);
        let (focal_length_35mm, focal_length_35mm_val) = focal_length_35mm_values(exif);
        let aperture = aperture_value(exif);
        let shutter = shutter_value(exif);
        let iso = iso_value(exif);
        let datetime = datetime_value(exif);
        let gps_data = gps_coordinates(exif);
        let geocoded = gps_data
            .as_ref()
            .and_then(|gps| reverse_geocode(gps.latitude, gps.longitude));

        let (fallback_location, fallback_country) = location_values(exif);

        let location = geocoded.clone().or(fallback_location);

        let country = geocoded
            .as_ref()
            .and_then(|name| extract_country(name))
            .or(fallback_country);

        let gps = gps_data.as_ref().map(|gps| gps.display.clone());

        Self {
            title,
            camera,
            lens,
            focal_length,
            focal_length_val,
            focal_length_35mm,
            focal_length_35mm_val,
            aperture,
            shutter,
            iso,
            datetime,
            location,
            country,
            gps,
        }
    }
}

fn build_caption(data: &ParsedExif) -> String {
    let mut output = String::new();

    // Emoji formatting follows the style requested by the user template.
    writeln!(output, "💭: {}", data.title.as_deref().unwrap_or("")).ok();
    writeln!(output, "——————————").ok();
    writeln!(output, "📸: {} / {}", data.camera, data.lens).ok();

    let use_full_frame = match (data.focal_length_val, data.focal_length_35mm_val) {
        (_, None) => true,
        (Some(f), Some(f35)) => (f - f35).abs() < 0.5,
        (None, Some(_)) => false,
    };

    let mut metrics: Vec<String> = Vec::new();

    if use_full_frame {
        if let Some(value) = data.focal_length.clone() {
            metrics.push(value);
        }
    } else if let Some(value) = data.focal_length_35mm.clone() {
        metrics.push(value);
    } else if let Some(value) = data.focal_length.clone() {
        metrics.push(value);
    }

    if let Some(value) = data.aperture.clone() {
        metrics.push(value);
    }
    if let Some(value) = data.shutter.clone() {
        metrics.push(value);
    }
    if let Some(value) = data.iso.clone() {
        metrics.push(value);
    }

    if metrics.is_empty() {
        writeln!(output, "📝: Parameters Unknown").ok();
    } else {
        writeln!(output, "📝: {}", metrics.join(", ")).ok();
    }

    writeln!(
        output,
        "📅: {}",
        data.datetime.as_deref().unwrap_or("Unknown")
    )
    .ok();

    match (data.location.as_deref(), data.country.as_deref()) {
        (Some(location), Some(country)) => {
            writeln!(output, "🗺️: {}, {}", location, country).ok();
        }
        (Some(location), None) => {
            writeln!(output, "🗺️: {}", location).ok();
        }
        (None, Some(country)) => {
            writeln!(output, "🗺️: {}", country).ok();
        }
        (None, None) => {}
    }

    if let Some(gps) = data.gps.as_deref() {
        writeln!(output, "📍: {}", gps).ok();
    }

    while output.ends_with('\n') {
        output.pop();
    }

    output
}

fn build_empty_caption() -> String {
    let data = ParsedExif {
        title: None,
        camera: String::from("Unknown Camera"),
        lens: String::from("Unknown Lens"),
        focal_length: None,
        focal_length_val: None,
        focal_length_35mm: None,
        focal_length_35mm_val: None,
        aperture: None,
        shutter: None,
        iso: None,
        datetime: None,
        location: None,
        country: None,
        gps: None,
    };

    build_caption(&data)
}

fn first_string(exif: &Exif, tags: &[Tag]) -> Option<String> {
    tags.iter()
        .filter_map(|tag| find_field(exif, *tag))
        .find_map(|field| field_to_string(field))
        .map(|s| s.trim().trim_matches('\0').to_string())
        .filter(|s| !s.is_empty())
}

fn find_field<'a>(exif: &'a Exif, tag: Tag) -> Option<&'a Field> {
    if let Some(field) = exif.get_field(tag, In::PRIMARY) {
        return Some(field);
    }

    for field in exif.fields() {
        if field.tag == tag {
            return Some(field);
        }
    }

    None
}

fn field_to_string(field: &Field) -> Option<String> {
    match &field.value {
        Value::Ascii(values) => values
            .first()
            .and_then(|bytes| String::from_utf8(bytes.clone()).ok())
            .map(|s| s.trim_end_matches('\0').to_string()),
        Value::Undefined(bytes, _) => {
            if bytes.is_empty() {
                None
            } else {
                let text = String::from_utf8_lossy(bytes)
                    .trim_matches('\0')
                    .trim()
                    .to_string();
                if text.is_empty() { None } else { Some(text) }
            }
        }
        _ => {
            let text = field.display_value().to_string();
            let trimmed = text.trim_matches('\0').trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        }
    }
}

fn focal_length_values(exif: &Exif) -> (Option<String>, Option<f64>) {
    let field = find_field(exif, Tag::FocalLength);
    if let Some(field) = field {
        if let Value::Rational(values) = &field.value {
            if let Some(rational) = values.first() {
                let value = rational.to_f64();
                if value.is_finite() {
                    let display = if (value - value.round()).abs() < 0.1 {
                        format!("{:.0}mm", value.round())
                    } else {
                        format!("{:.1}mm", value)
                    };
                    return (Some(display), Some(value));
                }
            }
        }
    }
    (None, None)
}

fn focal_length_35mm_values(exif: &Exif) -> (Option<String>, Option<f64>) {
    let field = find_field(exif, Tag::FocalLengthIn35mmFilm);
    if let Some(field) = field {
        if let Some(value) = field.value.get_uint(0) {
            let value = value as f64;
            let display = format!("{:.0}mm (35mm eq)", value);
            return (Some(display), Some(value));
        }
    }
    (None, None)
}

fn aperture_value(exif: &Exif) -> Option<String> {
    let field = find_field(exif, Tag::FNumber).or_else(|| find_field(exif, Tag::ApertureValue));
    if let Some(field) = field {
        if let Value::Rational(values) = &field.value {
            if let Some(rational) = values.first() {
                let value = rational.to_f64();
                if value.is_finite() {
                    return Some(format_fnumber(value));
                }
            }
        }
    }
    None
}

fn shutter_value(exif: &Exif) -> Option<String> {
    let field =
        find_field(exif, Tag::ExposureTime).or_else(|| find_field(exif, Tag::ShutterSpeedValue));
    if let Some(field) = field {
        if let Value::Rational(values) = &field.value {
            if let Some(rational) = values.first() {
                let value = rational.to_f64();
                if !value.is_finite() || value <= 0.0 {
                    return None;
                }

                if value >= 1.0 {
                    let rounded = value.round();
                    if (value - rounded).abs() < 0.01 {
                        return Some(format!("{rounded:.0}s"));
                    }
                    let precise = (value * 100.0).round() / 100.0;
                    return Some(format!("{precise:.2}s"));
                }

                let reciprocal = (1.0 / value).round();
                let approx = 1.0 / reciprocal;
                if (approx - value).abs() < 0.01 && reciprocal <= 8000.0 {
                    return Some(format!("1/{:.0}s", reciprocal));
                }

                let precise = (value * 1000.0).round() / 1000.0;
                return Some(format!("{precise:.3}s"));
            }
        }
    }
    None
}

fn iso_value(exif: &Exif) -> Option<String> {
    let field = find_field(exif, Tag::PhotographicSensitivity)
        .or_else(|| find_field(exif, Tag::ISOSpeed))
        .or_else(|| find_field(exif, Tag::ISOSpeedLatitudeyyy))
        .or_else(|| find_field(exif, Tag::ISOSpeedLatitudezzz));
    if let Some(field) = field {
        if let Some(value) = field.value.get_uint(0) {
            return Some(format!("ISO {value}"));
        }
    }
    None
}

fn datetime_value(exif: &Exif) -> Option<String> {
    let field = find_field(exif, Tag::DateTimeOriginal)
        .or_else(|| find_field(exif, Tag::DateTimeDigitized))
        .or_else(|| find_field(exif, Tag::DateTime));
    if let Some(field) = field {
        if let Value::Ascii(values) = &field.value {
            if let Some(bytes) = values.first() {
                if let Ok(text) = String::from_utf8(bytes.clone()) {
                    return Some(format_datetime(&text));
                }
            }
        }
    }
    None
}

fn location_values(exif: &Exif) -> (Option<String>, Option<String>) {
    let location = first_string(exif, &[Tag::GPSAreaInformation]);
    (location, None)
}

fn gps_coordinates(exif: &Exif) -> Option<GpsData> {
    let lat = find_field(exif, Tag::GPSLatitude)?;
    let lon = find_field(exif, Tag::GPSLongitude)?;

    let lat_value = gps_coordinate(&lat.value)?;
    let lon_value = gps_coordinate(&lon.value)?;

    let lat_ref = find_field(exif, Tag::GPSLatitudeRef)
        .and_then(|field| field_to_string(field))
        .unwrap_or_else(|| String::from("N"));
    let lon_ref = find_field(exif, Tag::GPSLongitudeRef)
        .and_then(|field| field_to_string(field))
        .unwrap_or_else(|| String::from("E"));

    let lat_dir = normalized_gps_ref(&lat_ref, 'N');
    let lon_dir = normalized_gps_ref(&lon_ref, 'E');

    let signed_lat = if lat_dir == 'S' {
        -lat_value
    } else {
        lat_value
    };
    let signed_lon = if lon_dir == 'W' {
        -lon_value
    } else {
        lon_value
    };

    let display = format!(
        "{:.6}° {}, {:.6}° {}",
        lat_value.abs(),
        lat_dir,
        lon_value.abs(),
        lon_dir
    );

    Some(GpsData {
        display,
        latitude: signed_lat,
        longitude: signed_lon,
    })
}

fn gps_coordinate(value: &Value) -> Option<f64> {
    if let Value::Rational(values) = value {
        if values.len() >= 3 {
            let degrees = values[0].to_f64();
            let minutes = values[1].to_f64();
            let seconds = values[2].to_f64();
            if [degrees, minutes, seconds]
                .iter()
                .all(|component| component.is_finite())
            {
                return Some(degrees + minutes / 60.0 + seconds / 3600.0);
            }
        }
    }
    None
}

fn normalized_gps_ref(reference: &str, default: char) -> char {
    reference
        .chars()
        .find(|c| c.is_ascii_alphabetic())
        .map(|c| c.to_ascii_uppercase())
        .filter(|c| matches!(c, 'N' | 'S' | 'E' | 'W'))
        .unwrap_or(default)
}

fn reverse_geocode(lat: f64, lon: f64) -> Option<String> {
    let url = format!(
        "{}?lat={:.6}&lon={:.6}&addressdetails=0&accept-language=zh-cn&format=json",
        NOMINATIM_ENDPOINT, lat, lon
    );

    let client = Client::new();
    let response = match client
        .get(url)
        .header("User-Agent", NOMINATIM_USER_AGENT)
        .send()
    {
        Ok(resp) => resp,
        Err(err) => {
            warn!(
                "Reverse geocoding request failed for coordinates ({:.6}, {:.6}): {}",
                lat, lon, err
            );
            return None;
        }
    };

    let response = match response.error_for_status() {
        Ok(resp) => resp,
        Err(err) => {
            warn!(
                "Reverse geocoding returned error for coordinates ({:.6}, {:.6}): {}",
                lat, lon, err
            );
            return None;
        }
    };

    let body = match response.text() {
        Ok(text) => text,
        Err(err) => {
            warn!(
                "Failed to read reverse geocoding response for coordinates ({:.6}, {:.6}): {}",
                lat, lon, err
            );
            return None;
        }
    };

    let value: JsonValue = match serde_json::from_str(&body) {
        Ok(json) => json,
        Err(err) => {
            warn!(
                "Failed to parse reverse geocoding JSON for coordinates ({:.6}, {:.6}): {}",
                lat, lon, err
            );
            return None;
        }
    };

    value
        .get("display_name")
        .and_then(|field| field.as_str())
        .map(|name| name.to_string())
}

fn extract_country(location: &str) -> Option<String> {
    location
        .split(',')
        .map(|part| part.trim())
        .rev()
        .find(|part| !part.is_empty())
        .map(|part| part.to_string())
}

fn lens_specification(exif: &Exif) -> Option<String> {
    let field = find_field(exif, Tag::LensSpecification)?;
    if let Value::Rational(values) = &field.value {
        if values.len() >= 4 {
            let focal_min = values[0].to_f64();
            let focal_max = values[1].to_f64();
            let aperture_min = values[2].to_f64();
            let aperture_max = values[3].to_f64();

            let focal = if (focal_min - focal_max).abs() < 0.5 {
                format!("{:.0}mm", focal_min.round())
            } else {
                format!("{:.0}-{:.0}mm", focal_min.round(), focal_max.round())
            };

            let aperture = if (aperture_min - aperture_max).abs() < 0.1 {
                format_fnumber(aperture_min)
            } else {
                format!(
                    "{}-{}",
                    format_fnumber(aperture_min),
                    format_fnumber(aperture_max)
                )
            };

            return Some(format!("{} {}", focal, aperture));
        }
    }
    None
}

fn format_datetime(input: &str) -> String {
    let trimmed = input.trim_matches('\0').trim();
    if trimmed.len() >= 19 {
        let date = &trimmed[0..10].replace(':', "-");
        let time = &trimmed[11..19];
        format!("{} {}", date, time)
    } else {
        trimmed.to_string()
    }
}

fn format_fnumber(value: f64) -> String {
    if !value.is_finite() {
        return String::from("f/--");
    }

    let rounded_whole = value.round();
    if (value - rounded_whole).abs() < 0.05 {
        format!("f/{:.0}", rounded_whole)
    } else {
        format!("f/{:.1}", value)
    }
}
