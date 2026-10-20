output "api_base_url" {
  value = local.public_base_url
}

output "oauth_callback_url" {
  description = "Register this as the GitHub OAuth App callback URL."
  value       = "${local.public_base_url}/login/github/callback"
}

output "registry_aws_principal_arn" {
  description = "Principal that S3 bucket owners must trust (with their ExternalId)."
  value       = aws_iam_role.lambda.arn
}

output "secrets_arn" {
  value = aws_secretsmanager_secret.registry.arn
}

output "table_name" {
  value = aws_dynamodb_table.registry.name
}

output "custom_domain_target" {
  description = "CNAME target for custom_domain_name (when set)."
  value       = var.custom_domain_name != "" ? aws_api_gateway_domain_name.registry[0].regional_domain_name : null
}
