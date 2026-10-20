output "d1_database_id" {
  description = "Feed into scripts/render-wrangler.mjs (D1_DATABASE_ID)."
  value       = cloudflare_d1_database.registry.id
}

output "d1_database_name" {
  value = cloudflare_d1_database.registry.name
}

output "public_base_url" {
  value = "https://${var.hostname}"
}

output "registry_aws_principal_arn" {
  description = "Set as REGISTRY_AWS_PRINCIPAL_ARN; S3 bucket owners trust this principal."
  value       = var.enable_aws_sts_principal ? aws_iam_user.sts[0].arn : null
}
