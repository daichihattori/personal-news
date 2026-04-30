resource "google_artifact_registry_repository" "main" {
  location      = var.region
  repository_id = "personal-news"
  format        = "DOCKER"
  description   = "Docker images for personal-news app"

  depends_on = [google_project_service.apis]
}
