variable "project_id" {
  type        = string
  description = "GCP project ID"
}

variable "region" {
  type        = string
  default     = "asia-northeast1"
  description = "GCP region (default: Tokyo)"
}

variable "gemini_api_key" {
  type        = string
  sensitive   = true
  description = "Gemini API key (stored in Secret Manager)"
}

variable "gemini_model" {
  type    = string
  default = "gemini-2.0-flash"
}

variable "gemini_fallback_models" {
  type    = string
  default = "gemini-2.0-flash-lite"
}

variable "domain" {
  type        = string
  description = "Custom domain for the app (e.g. news.example.com). Used for managed SSL cert and URL map host rule."
}

variable "voicevox_min_instances" {
  type        = number
  default     = 0
  description = "Set to 1 to avoid cold starts (costs ~$5/month extra)"
}
