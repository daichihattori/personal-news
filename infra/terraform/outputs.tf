output "frontend_url" {
  description = "Public URL for the frontend"
  value       = google_cloud_run_v2_service.frontend.uri
}

output "backend_url" {
  description = "Public URL for the backend API"
  value       = google_cloud_run_v2_service.backend.uri
}

output "voicevox_url" {
  description = "Public URL for the VOICEVOX service"
  value       = google_cloud_run_v2_service.voicevox.uri
}

output "registry" {
  description = "Artifact Registry path for Docker images"
  value       = local.registry
}

output "data_bucket" {
  description = "GCS bucket name for persistent data"
  value       = google_storage_bucket.data.name
}
