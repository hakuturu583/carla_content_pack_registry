variable "environment" {
  description = "staging or production"
  type        = string
  validation {
    condition     = contains(["staging", "production"], var.environment)
    error_message = "environment must be staging or production."
  }
}

variable "cloudflare_account_id" {
  type = string
}

variable "zone_id" {
  description = "Cloudflare zone that hosts the registry hostname."
  type        = string
}

variable "hostname" {
  description = "Public hostname of the registry, e.g. registry.example.org"
  type        = string
}

variable "d1_location_hint" {
  type    = string
  default = "apac"
}

# --- WAF / bot protection -----------------------------------------------------
# Defaults work on the Cloudflare Free plan (1 rate-limiting rule, 10 s period).

variable "ratelimit_requests_per_period" {
  type    = number
  default = 100
}

variable "ratelimit_period_seconds" {
  type    = number
  default = 10
}

variable "ratelimit_mitigation_seconds" {
  type    = number
  default = 10
}

variable "enable_bot_fight_mode" {
  type    = bool
  default = true
}

variable "enable_managed_waf" {
  description = "Deploy the Cloudflare Managed Ruleset (requires Pro plan or higher)."
  type        = bool
  default     = false
}

# --- AWS principal for STS AssumeRole (S3 buckets) ------------------------------

variable "enable_aws_sts_principal" {
  description = "Create the IAM user the Worker uses to call sts:AssumeRole on user-owned S3 roles."
  type        = bool
  default     = false
}

variable "aws_region" {
  type    = string
  default = "us-east-1"
}

variable "assumable_role_arn_pattern" {
  description = "Which user roles the registry may assume. Narrow this (e.g. arn:aws:iam::*:role/ccpr-*) if you can require a naming convention."
  type        = string
  default     = "arn:aws:iam::*:role/*"
}
