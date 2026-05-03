output "lb_ip" {
  description = "Load balancer IP — point your DNS A record here"
  value       = google_compute_global_address.default.address
}

output "app_url" {
  description = "Application URL"
  value       = "https://${var.domain}"
}

output "frontend_url" {
  description = "Direct Cloud Run URL for the frontend (internal use)"
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
