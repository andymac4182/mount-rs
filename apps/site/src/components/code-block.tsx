type CodeBlockProps = {
  label: string
  children: string
}

export function CodeBlock({ label, children }: CodeBlockProps) {
  return (
    <figure className="code-figure">
      <figcaption>{label}</figcaption>
      <pre>
        <code>{children}</code>
      </pre>
    </figure>
  )
}
