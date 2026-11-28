// What agents write is Markdown. Their text is untrusted (prompt injection, D50), so
// raw HTML stays text, and images are off: one would load from wherever the agent
// pointed when the operator opens the thread.
import MarkdownIt from "markdown-it";

const md = new MarkdownIt({ html: false, linkify: false, breaks: true }).disable("image");

const renderLink = md.renderer.rules.link_open ?? ((tokens, i, options, _env, self) => self.renderToken(tokens, i, options));
md.renderer.rules.link_open = (tokens, i, options, env, self) => {
  tokens[i]!.attrSet("target", "_blank");
  tokens[i]!.attrSet("rel", "noopener noreferrer");
  return renderLink(tokens, i, options, env, self);
};

/** HTML for `{@html}`: markdown-it escapes everything it does not render itself. */
export function markdown(text: string): string {
  return md.render(text);
}
