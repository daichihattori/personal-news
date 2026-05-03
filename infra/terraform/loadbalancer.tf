# ---------- Static IP ----------
resource "google_compute_global_address" "default" {
  name       = "personal-news-ip"
  depends_on = [google_project_service.apis]
}

# ---------- Managed SSL cert ----------
resource "google_compute_managed_ssl_certificate" "default" {
  name = "personal-news-cert"
  managed {
    domains = [var.domain]
  }
  depends_on = [google_project_service.apis]
}

# ---------- Serverless NEGs (Cloud Run) ----------
resource "google_compute_region_network_endpoint_group" "backend" {
  name                  = "personal-news-backend-neg"
  network_endpoint_type = "SERVERLESS"
  region                = var.region
  cloud_run {
    service = google_cloud_run_v2_service.backend.name
  }
}

resource "google_compute_region_network_endpoint_group" "frontend" {
  name                  = "personal-news-frontend-neg"
  network_endpoint_type = "SERVERLESS"
  region                = var.region
  cloud_run {
    service = google_cloud_run_v2_service.frontend.name
  }
}

# ---------- Backend services ----------
resource "google_compute_backend_service" "backend" {
  name                  = "personal-news-backend-svc"
  protocol              = "HTTPS"
  load_balancing_scheme = "EXTERNAL_MANAGED"
  timeout_sec           = 300

  backend {
    group = google_compute_region_network_endpoint_group.backend.id
  }
}

resource "google_compute_backend_service" "frontend" {
  name                  = "personal-news-frontend-svc"
  protocol              = "HTTPS"
  load_balancing_scheme = "EXTERNAL_MANAGED"
  enable_cdn            = true

  backend {
    group = google_compute_region_network_endpoint_group.frontend.id
  }
}

# ---------- URL Map ----------
resource "google_compute_url_map" "default" {
  name            = "personal-news-urlmap"
  default_service = google_compute_backend_service.frontend.id

  host_rule {
    hosts        = [var.domain]
    path_matcher = "paths"
  }

  path_matcher {
    name            = "paths"
    default_service = google_compute_backend_service.frontend.id

    path_rule {
      paths   = ["/api", "/api/*", "/audio", "/audio/*"]
      service = google_compute_backend_service.backend.id
    }
  }
}

# ---------- HTTPS ----------
resource "google_compute_target_https_proxy" "default" {
  name             = "personal-news-https-proxy"
  url_map          = google_compute_url_map.default.id
  ssl_certificates = [google_compute_managed_ssl_certificate.default.id]
}

resource "google_compute_global_forwarding_rule" "https" {
  name                  = "personal-news-https"
  target                = google_compute_target_https_proxy.default.id
  port_range            = "443"
  ip_address            = google_compute_global_address.default.address
  load_balancing_scheme = "EXTERNAL_MANAGED"
}

# ---------- HTTP → HTTPS redirect ----------
resource "google_compute_url_map" "redirect" {
  name = "personal-news-http-redirect"
  default_url_redirect {
    https_redirect = true
    strip_query    = false
  }
}

resource "google_compute_target_http_proxy" "redirect" {
  name    = "personal-news-http-proxy"
  url_map = google_compute_url_map.redirect.id
}

resource "google_compute_global_forwarding_rule" "http" {
  name                  = "personal-news-http"
  target                = google_compute_target_http_proxy.redirect.id
  port_range            = "80"
  ip_address            = google_compute_global_address.default.address
  load_balancing_scheme = "EXTERNAL_MANAGED"
}
