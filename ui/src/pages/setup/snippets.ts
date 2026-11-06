/** First-request snippets: one pure function, so the copy and the code block never drift. */

export type SnippetLang = "curl" | "python" | "typescript";

export const SNIPPET_LANGS: ReadonlyArray<{ id: SnippetLang; label: string }> = [
  { id: "curl", label: "cURL" },
  { id: "python", label: "Python" },
  { id: "typescript", label: "TypeScript" },
];

/** The key a snippet carries when the resume path has no plaintext to show. */
export const KEY_PLACEHOLDER = "YOUR_API_KEY";

export function snippetFor(lang: SnippetLang, baseUrl: string, key: string, model: string): string {
  const base = baseUrl.replace(/\/$/, "");
  const secret = key || KEY_PLACEHOLDER;
  if (lang === "python") {
    return [
      "from openai import OpenAI",
      "",
      `client = OpenAI(base_url="${base}", api_key="${secret}")`,
      "reply = client.chat.completions.create(",
      `    model="${model}",`,
      '    messages=[{"role": "user", "content": "ping"}],',
      ")",
      "print(reply.choices[0].message.content)",
    ].join("\n");
  }
  if (lang === "typescript") {
    return [
      'import OpenAI from "openai";',
      "",
      `const client = new OpenAI({ baseURL: "${base}", apiKey: "${secret}" });`,
      "const reply = await client.chat.completions.create({",
      `  model: "${model}",`,
      '  messages: [{ role: "user", content: "ping" }],',
      "});",
      "console.log(reply.choices[0].message.content);",
    ].join("\n");
  }
  return [
    `curl ${base}/chat/completions \\`,
    `  -H "Authorization: Bearer ${secret}" \\`,
    '  -H "Content-Type: application/json" \\',
    `  -d '{"model":"${model}","messages":[{"role":"user","content":"ping"}]}'`,
  ].join("\n");
}
