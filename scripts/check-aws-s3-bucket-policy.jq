def values_array($value):
  if $value == null then []
  elif ($value | type) == "array" then $value
  else [$value]
  end;

def exact_values($value; $expected):
  (values_array($value) | sort) == ($expected | sort);

def aws_principal($role):
  (.Principal | type == "object")
  and ((.Principal | keys | sort) == ["AWS"])
  and (.Principal.AWS == $role);

def owned_prefix_condition($prefix):
  exact_values(.Condition.StringLike["s3:prefix"]; [$prefix, ($prefix + "/*")]);

def allow_statement($sid; $role; $actions; $resources; $condition):
  any(.Statement[]?;
    .Sid == $sid
    and .Effect == "Allow"
    and aws_principal($role)
    and exact_values(.Action; $actions)
    and exact_values(.Resource; $resources)
    and $condition
  );

def allow_prefix_statement($sid; $role; $actions; $bucket; $prefix):
  any(.Statement[]?;
    .Sid == $sid
    and .Effect == "Allow"
    and aws_principal($role)
    and exact_values(.Action; $actions)
    and exact_values(.Resource; [$bucket])
    and owned_prefix_condition($prefix)
  );

def transport_deny($bucket_arn):
  any(.Statement[]?;
    .Sid == "DenyInsecureTransport"
    and .Effect == "Deny"
    and .Principal == "*"
    and exact_values(.Action; ["s3:*"])
    and exact_values(.Resource; [$bucket_arn, ($bucket_arn + "/*")])
    and ((.Condition.Bool["aws:SecureTransport"] | tostring) == "false")
  );

($bucket_arn) as $bucket |
[
  (.Statement | type == "array" and length == 5),
  transport_deny($bucket),
  allow_prefix_statement(
    "RuntimeListOwnedPrefix";
    $runtime_role;
    ["s3:ListBucket"];
    $bucket;
    $owned_prefix
  ),
  allow_statement(
    "RuntimeObjectAccess";
    $runtime_role;
    ["s3:AbortMultipartUpload", "s3:GetObject", "s3:PutObject"];
    [($bucket + "/" + $owned_prefix + "/*")];
    true
  ),
  allow_prefix_statement(
    "MaintenanceListOwnedPrefix";
    $maintenance_role;
    ["s3:ListBucket", "s3:ListBucketMultipartUploads", "s3:ListBucketVersions"];
    $bucket;
    $owned_prefix
  ),
  allow_statement(
    "MaintenanceObjectAccess";
    $maintenance_role;
    ["s3:AbortMultipartUpload", "s3:DeleteObject", "s3:DeleteObjectVersion", "s3:GetObject", "s3:GetObjectVersion", "s3:ListMultipartUploadParts", "s3:PutObject"];
    [($bucket + "/" + $owned_prefix + "/*")];
    true
  )
] | all
