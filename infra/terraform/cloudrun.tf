locals {
  registry = "${var.region}-docker.pkg.dev/${var.project_id}/personal-news"
}

# ---------- VOICEVOX ----------
resource "google_cloud_run_v2_service" "voicevox" {
  name     = "personal-news-voicevox"
  location = var.region
  ingress  = "INGRESS_TRAFFIC_ALL"

  template {
    scaling {
      min_instance_count = var.voicevox_min_instances
      max_instance_count = 1
    }

    containers {
      image = "voicevox/voicevox_engine:cpu-ubuntu20.04-latest"

      ports {
        container_port = 50021
      }

      resources {
        limits = {
          cpu    = "2"
          memory = "2Gi"
        }
        startup_cpu_boost = true
      }
    }
  }

  depends_on = [google_project_service.apis]
}

# ---------- Backend ----------
resource "google_cloud_run_v2_service" "backend" {
  name     = "personal-news-backend"
  location = var.region
  ingress  = "INGRESS_TRAFFIC_ALL"

  template {
    service_account = google_service_account.backend.email

    # SQLite on GCS FUSE does not support concurrent writers;
    # keep max_instance_count = 1 for this personal-use service.
    scaling {
      min_instance_count = 0
      max_instance_count = 1
    }

    volumes {
      name = "gcs-data"
      gcs {
        bucket    = google_storage_bucket.data.name
        read_only = false
      }
    }

    containers {
      image = "${local.registry}/backend:latest"

      ports {
        container_port = 8080
      }

      volume_mounts {
        name       = "gcs-data"
        mount_path = "/data"
      }

      env {
        name  = "APP_ENV"
        value = "production"
      }
      env {
        name  = "DATABASE_PATH"
        value = "/data/db.sqlite"
      }
      env {
        name  = "GEMINI_MODEL"
        value = var.gemini_model
      }
      env {
        name  = "GEMINI_FALLBACK_MODELS"
        value = var.gemini_fallback_models
      }
      env {
        name  = "VOICEVOX_BASE_URL"
        value = google_cloud_run_v2_service.voicevox.uri
      }
      env {
        name = "GEMINI_API_KEY"
        value_source {
          secret_key_ref {
            secret  = google_secret_manager_secret.gemini_api_key.secret_id
            version = "latest"
          }
        }
      }

      resources {
        limits = {
          cpu    = "1"
          memory = "512Mi"
        }
        startup_cpu_boost = true
      }

      startup_probe {
        http_get {
          path = "/api/health"
          port = 8080
        }
        initial_delay_seconds = 5
        period_seconds        = 10
        failure_threshold     = 3
      }
    }
  }

  depends_on = [
    google_project_service.apis,
    google_cloud_run_v2_service.voicevox,
    google_secret_manager_secret_version.gemini_api_key,
  ]
}

# ---------- Frontend ----------
resource "google_cloud_run_v2_service" "frontend" {
  name     = "personal-news-frontend"
  location = var.region
  ingress  = "INGRESS_TRAFFIC_ALL"

  template {
    scaling {
      min_instance_count = 0
      max_instance_count = 3
    }

    containers {
      image = "${local.registry}/frontend:latest"

      ports {
        container_port = 8080
      }

      env {
        name  = "BACKEND_URL"
        value = google_cloud_run_v2_service.backend.uri
      }

      resources {
        limits = {
          cpu    = "1"
          memory = "256Mi"
        }
      }
    }
  }

  depends_on = [
    google_project_service.apis,
    google_cloud_run_v2_service.backend,
  ]
}
