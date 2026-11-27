//! Administrative reference-source discovery, independent of private charts.

use axum::{
    http::{header, StatusCode},
    response::IntoResponse,
    Json,
};

/// Returns only the raw source eligible for authenticated local acquisition.
pub(crate) async fn sources(
    _session: crate::api::AdminSession,
) -> Result<impl IntoResponse, StatusCode> {
    let mut manifest: ephemeris::EphemerisManifest =
        serde_json::from_str(include_str!("../../ephemeris/manifest.json"))
            .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    // Generated Horizons kernels have a separate, unresolved redistribution gate.
    manifest.files.retain(|file| file.name == "de440.bsp");
    manifest
        .validate()
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    if manifest.files.len() != 1 {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    Ok((
        [(header::CACHE_CONTROL, "private, no-store")],
        Json(manifest),
    ))
}

#[cfg(test)]
mod tests {}
