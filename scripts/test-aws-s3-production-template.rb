#!/usr/bin/env ruby
# frozen_string_literal: true

# Credential-free structural contract test for the reviewable W25 CloudFormation
# template. It does not call AWS and does not create or mutate any resource.

require "psych"

template_path = ARGV.fetch(0, "infra/aws-s3-production.yaml")

def fail_contract(message)
  warn "AWS_S3_TEMPLATE_CONTRACT_FAILED #{message}"
  exit 1
end

def require_value(condition, message)
  fail_contract(message) unless condition
end

template = Psych.safe_load_file(template_path, aliases: true)
require_value(template.is_a?(Hash), "template_root_is_not_a_mapping")
require_value(template["AWSTemplateFormatVersion"] == "2010-09-09", "template_version")

parameters = template.fetch("Parameters") { fail_contract("parameters_missing") }
required_parameters = %w[
  BucketName
  OwnedPrefix
  RuntimeRoleArn
  MaintenanceRoleArn
  EncryptionAlgorithm
  KmsKeyArn
  ObjectExpirationDays
  IncompleteMultipartAbortDays
]
require_value(parameters.keys.sort == required_parameters.sort, "parameter_set")
require_value(
  parameters.fetch("BucketName")["AllowedPattern"] == "^[a-z0-9][a-z0-9.-]{1,61}[a-z0-9]$",
  "bucket_name_constraint"
)
require_value(
  parameters.fetch("OwnedPrefix")["AllowedPattern"] ==
    "^[A-Za-z0-9](?:[A-Za-z0-9._-]*[A-Za-z0-9])?(?:/[A-Za-z0-9](?:[A-Za-z0-9._-]*[A-Za-z0-9])?)*$",
  "owned_prefix_constraint"
)
require_value(parameters.fetch("EncryptionAlgorithm")["AllowedValues"] == ["AES256", "aws:kms"], "encryption_choices")
require_value(parameters.fetch("KmsKeyArn")["Default"] == "", "kms_key_default")
require_value(parameters.fetch("ObjectExpirationDays")["MinValue"] == 1, "expiration_lower_bound")
require_value(parameters.fetch("ObjectExpirationDays")["MaxValue"] == 3650, "expiration_upper_bound")
require_value(parameters.fetch("IncompleteMultipartAbortDays")["MinValue"] == 1, "multipart_lower_bound")
require_value(parameters.fetch("IncompleteMultipartAbortDays")["MaxValue"] == 30, "multipart_upper_bound")

conditions = template.fetch("Conditions") { fail_contract("conditions_missing") }
require_value(conditions["UseCustomerManagedKms"] == ["EncryptionAlgorithm", "aws:kms"], "kms_condition")

rules = template.fetch("Rules") { fail_contract("rules_missing") }
role_rule = rules.fetch("RuntimeAndMaintenanceRolesMustDiffer")
require_value(
  role_rule.fetch("Assertions") == [
    {
      "Assert" => [["RuntimeRoleArn", "MaintenanceRoleArn"]],
      "AssertDescription" => "RuntimeRoleArn and MaintenanceRoleArn must be different roles."
    }
  ],
  "role_separation_rule"
)
require_value(
  rules.fetch("KmsKeyRequiredWhenSelected") == {
    "RuleCondition" => ["EncryptionAlgorithm", "aws:kms"],
    "Assertions" => [
      {
        "Assert" => [["KmsKeyArn", ""]],
        "AssertDescription" => "KmsKeyArn is required when EncryptionAlgorithm is aws:kms."
      }
    ]
  },
  "kms_required_rule"
)
require_value(
  rules.fetch("KmsKeyMustBeOmittedForSseS3") == {
    "RuleCondition" => ["EncryptionAlgorithm", "AES256"],
    "Assertions" => [
      {
        "Assert" => ["KmsKeyArn", ""],
        "AssertDescription" => "KmsKeyArn must be empty when EncryptionAlgorithm is AES256."
      }
    ]
  },
  "kms_omitted_rule"
)

resources = template.fetch("Resources") { fail_contract("resources_missing") }
bucket = resources.fetch("MountRsBucket")
require_value(bucket["Type"] == "AWS::S3::Bucket", "bucket_type")
require_value(bucket["DeletionPolicy"] == "Retain", "bucket_deletion_policy")
require_value(bucket["UpdateReplacePolicy"] == "Retain", "bucket_update_replace_policy")
bucket_properties = bucket.fetch("Properties")
require_value(
  bucket_properties.fetch("OwnershipControls") == { "Rules" => [{ "ObjectOwnership" => "BucketOwnerEnforced" }] },
  "bucket_ownership"
)
require_value(
  bucket_properties.fetch("PublicAccessBlockConfiguration") == {
    "BlockPublicAcls" => true,
    "BlockPublicPolicy" => true,
    "IgnorePublicAcls" => true,
    "RestrictPublicBuckets" => true
  },
  "bucket_public_access_block"
)
require_value(
  bucket_properties.fetch("VersioningConfiguration") == { "Status" => "Enabled" },
  "bucket_versioning"
)
require_value(
  bucket_properties.fetch("BucketEncryption") == {
    "ServerSideEncryptionConfiguration" => [
      {
        "ServerSideEncryptionByDefault" => {
          "SSEAlgorithm" => "EncryptionAlgorithm",
          "KMSMasterKeyID" => ["UseCustomerManagedKms", "KmsKeyArn", "AWS::NoValue"]
        }
      }
    ]
  },
  "bucket_encryption"
)
require_value(
  bucket_properties.fetch("LifecycleConfiguration") == {
    "Rules" => [
      {
        "Id" => "MountRsOwnedPrefixRetention",
        "Status" => "Enabled",
        "Prefix" => "${OwnedPrefix}/",
        "ExpirationInDays" => "ObjectExpirationDays",
        "NoncurrentVersionExpiration" => { "NoncurrentDays" => "ObjectExpirationDays" },
        "AbortIncompleteMultipartUpload" => { "DaysAfterInitiation" => "IncompleteMultipartAbortDays" }
      }
    ]
  },
  "bucket_lifecycle"
)

policy = resources.fetch("MountRsBucketPolicy").fetch("Properties").fetch("PolicyDocument")
require_value(policy["Version"] == "2012-10-17", "policy_version")
statements = policy.fetch("Statement")
require_value(statements.length == 5, "policy_statement_count")
require_value(
  statements.map { |statement| statement.fetch("Sid") } == [
    "DenyInsecureTransport",
    "RuntimeListOwnedPrefix",
    "RuntimeObjectAccess",
    "MaintenanceListOwnedPrefix",
    "MaintenanceObjectAccess"
  ],
  "policy_statement_order"
)

transport = statements.fetch(0)
require_value(
  transport == {
    "Sid" => "DenyInsecureTransport",
    "Effect" => "Deny",
    "Principal" => "*",
    "Action" => "s3:*",
    "Resource" => ["MountRsBucket.Arn", "${MountRsBucket.Arn}/*"],
    "Condition" => { "Bool" => { "aws:SecureTransport" => false } }
  },
  "transport_deny"
)

runtime_list, runtime_objects, maintenance_list, maintenance_objects = statements.drop(1)
prefix_condition = { "StringLike" => { "s3:prefix" => ["${OwnedPrefix}", "${OwnedPrefix}/*"] } }
require_value(
  runtime_list == {
    "Sid" => "RuntimeListOwnedPrefix",
    "Effect" => "Allow",
    "Principal" => { "AWS" => "RuntimeRoleArn" },
    "Action" => "s3:ListBucket",
    "Resource" => "MountRsBucket.Arn",
    "Condition" => prefix_condition
  },
  "runtime_list_policy"
)
require_value(
  runtime_objects == {
    "Sid" => "RuntimeObjectAccess",
    "Effect" => "Allow",
    "Principal" => { "AWS" => "RuntimeRoleArn" },
    "Action" => ["s3:AbortMultipartUpload", "s3:GetObject", "s3:PutObject"],
    "Resource" => "${MountRsBucket.Arn}/${OwnedPrefix}/*"
  },
  "runtime_object_policy"
)
require_value(
  maintenance_list == {
    "Sid" => "MaintenanceListOwnedPrefix",
    "Effect" => "Allow",
    "Principal" => { "AWS" => "MaintenanceRoleArn" },
    "Action" => ["s3:ListBucket", "s3:ListBucketMultipartUploads", "s3:ListBucketVersions"],
    "Resource" => "MountRsBucket.Arn",
    "Condition" => prefix_condition
  },
  "maintenance_list_policy"
)
require_value(
  maintenance_objects == {
    "Sid" => "MaintenanceObjectAccess",
    "Effect" => "Allow",
    "Principal" => { "AWS" => "MaintenanceRoleArn" },
    "Action" => [
      "s3:AbortMultipartUpload",
      "s3:DeleteObject",
      "s3:DeleteObjectVersion",
      "s3:GetObject",
      "s3:GetObjectVersion",
      "s3:ListMultipartUploadParts",
      "s3:PutObject"
    ],
    "Resource" => "${MountRsBucket.Arn}/${OwnedPrefix}/*"
  },
  "maintenance_object_policy"
)

puts "AWS_S3_TEMPLATE_CONTRACT_PASS path=#{template_path} statements=#{statements.length}"
