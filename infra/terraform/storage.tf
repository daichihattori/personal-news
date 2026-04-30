resource "google_storage_bucket" "data" {
  name          = "${var.project_id}-personal-news-data"
  location      = var.region
  force_destroy = false

  uniform_bucket_level_access = true

  # 生成した音声ファイルは90日後に自動削除（容量節約）
  lifecycle_rule {
    condition {
      age            = 90
      matches_suffix = [".wav"]
    }
    action {
      type = "Delete"
    }
  }

  depends_on = [google_project_service.apis]
}
