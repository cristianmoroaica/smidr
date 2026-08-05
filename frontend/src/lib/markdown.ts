import { marked } from 'marked';
import DOMPurify from 'dompurify';

/**
 * Render untrusted markdown to sanitized HTML for `{@html}`.
 *
 * Everything rendered this way — model output, tool results, the spec
 * artifact — originates outside the app, so this is the single sanitization
 * boundary. Keep it that way: call this rather than reaching for
 * `marked.parse` directly, so the DOMPurify step can never be dropped at one
 * call site while surviving at another.
 */
export function renderMarkdown(content: string): string {
  return DOMPurify.sanitize(marked.parse(content, { async: false }) as string);
}
