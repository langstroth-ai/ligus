/// Composer attachment helpers. Attachments live in the message body as
/// `[Attached: <abs path>]` markers (matching the Rust side), so the transcript
/// can split them back out for display and agents read the path directly.

export function attachmentMarker(path: string): string {
  return `[Attached: ${path}]`;
}

/// `[Attachment: <id>]`: where a shared file sits in a message body (issue
/// #118). Only a placement hint: what the file is and who shared it come from
/// the signed share (`listAttachments`), never from the text.
const SHARED_MARKER = /\[Attachment: ([0-9a-fA-F-]{36})\]/g;

/// Split a body into display text, device-local path attachments, and shared
/// attachment ids. Shared markers are stripped first, then plain
/// `[Attached: <path>]` markers.
export function splitAttachments(body: string): {
  text: string;
  paths: string[];
  attachmentIds: string[];
} {
  const paths: string[] = [];
  const attachmentIds: string[] = [];
  const text = body
    .replace(SHARED_MARKER, (_m, id: string) => {
      const lower = id.toLowerCase();
      if (!attachmentIds.includes(lower)) attachmentIds.push(lower);
      return "";
    })
    .replace(/\[Attached: ([^\]]+)\]/g, (_m, p: string) => {
      const trimmed = p.trim();
      if (trimmed) paths.push(trimmed);
      return "";
    })
    .replace(/\n{3,}/g, "\n\n")
    .trim();
  return { text, paths, attachmentIds };
}

/// Human file size.
export function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / (1024 * 1024)).toFixed(1)} MB`;
}

/// A srcdoc for an inert preview: no scripts, no network, nothing but the
/// data URL image or the escaped text. Rendered in `<iframe sandbox="">`.
export function previewSrcDoc(kind: "image" | "text", mime: string, dataBase64: string): string {
  const csp = "default-src 'none'; img-src data:; style-src 'unsafe-inline'";
  const head = `<!doctype html><html><head><meta http-equiv="Content-Security-Policy" content="${csp}"><meta charset="utf-8"><style>body{margin:0;font:12px ui-monospace,monospace;white-space:pre-wrap;word-break:break-word;padding:8px}img{max-width:100%;height:auto;display:block}</style></head><body>`;
  if (kind === "image") {
    const safeMime = /^image\/(png|jpeg|gif|webp|bmp)$/.test(mime) ? mime : "application/octet-stream";
    return `${head}<img alt="" src="data:${safeMime};base64,${dataBase64.replace(/[^A-Za-z0-9+/=]/g, "")}"></body></html>`;
  }
  let text = "";
  try {
    const bin = atob(dataBase64);
    const bytes = Uint8Array.from(bin, (c) => c.charCodeAt(0));
    text = new TextDecoder("utf-8", { fatal: false }).decode(bytes);
  } catch {
    text = "";
  }
  const escaped = text.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
  return `${head}${escaped}</body></html>`;
}

export function isImageType(contentType: string, name: string): boolean {
  return contentType.startsWith("image/") || isImagePath(name);
}

export function fileBaseName(path: string): string {
  const parts = path.split(/[\\/]/);
  return parts[parts.length - 1] || path;
}

export function isImagePath(path: string): boolean {
  return /\.(png|jpe?g|gif|webp|bmp|svg)$/i.test(path);
}
