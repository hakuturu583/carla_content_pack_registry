variable "environment" {
  type = string
  validation {
    condition     = contains(["staging", "production"], var.environment)
    error_message = "environment must be staging or production."
  }
}

variable "region" {
  type    = string
  default = "ap-northeast-1"
}

variable "lambda_zip" {
  description = "Path to the Lambda bundle zip built by CI (npm run build:lambda, then zip dist/lambda)."
  type        = string
  default     = "../../dist/lambda.zip"
}

variable "github_client_id" {
  description = "GitHub OAuth App client ID (public). The client secret lives in Secrets Manager."
  type        = string
}

variable "public_base_url" {
  description = "Externally visible base URL. Leave empty to use the API Gateway stage URL."
  type        = string
  default     = ""
}

variable "custom_domain_name" {
  description = "Optional custom domain for the API (requires certificate_arn in the same region)."
  type        = string
  default     = ""
}

variable "certificate_arn" {
  type    = string
  default = ""
}

variable "assumable_role_arn_pattern" {
  description = "Which user roles the registry may assume (sts:AssumeRole resource)."
  type        = string
  default     = "arn:aws:iam::*:role/*"
}

variable "lambda_reserved_concurrency" {
  description = "Upper bound on concurrent executions (cost/DoS guard). -1 = unreserved."
  type        = number
  default     = 20
}

variable "api_throttle_rate" {
  type    = number
  default = 50
}

variable "api_throttle_burst" {
  type    = number
  default = 100
}

variable "waf_rate_limit_per_5min" {
  description = "Requests per 5 minutes per client IP before AWS WAF blocks."
  type        = number
  default     = 1000
}

variable "enable_waf_bot_control" {
  description = "Add the AWS Bot Control managed rule group (additional cost)."
  type        = bool
  default     = false
}

variable "log_retention_days" {
  type    = number
  default = 90
}

variable "audit_retention_days" {
  type    = number
  default = 400
}

variable "alarm_email" {
  description = "Email for CloudWatch alarms and budget notifications (empty = no subscription)."
  type        = string
  default     = ""
}

variable "monthly_budget_usd" {
  type    = number
  default = 50
}
