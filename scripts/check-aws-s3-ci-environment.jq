any(.protection_rules[]?;
  ((.type // "" | ascii_downcase) == "required_reviewers")
  and (.reviewers | type == "array")
  and (.reviewers | length > 0)
  and (.prevent_self_review == true)
)
