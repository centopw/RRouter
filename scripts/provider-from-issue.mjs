#!/usr/bin/env node
/**
 * Turn a "API provider request" issue-form body into providers/community/<id>.json.
 *
 * Input: ISSUE_BODY (or a file path / stdin). Output: manifest path, plus `id` and
 * `path` in GITHUB_OUTPUT when running in Actions. Everything is validated here — the
 * generated file is data only and still needs a reviewed pull request.
 */
import fs from "node:fs";
import path from "node:path";

const body = process.env.ISSUE_BODY
  ?? (process.argv[2] && process.argv[2] !== "-" ? fs.readFileSync(process.argv[2], "utf8") : fs.readFileSync(0, "utf8"));

const sections = new Map();
for (const raw of body.split(/^### /m).slice(1)) {
  const [title, ...rest] = raw.split("\n");
  sections.set(title.trim().toLowerCase(), rest.join("\n").trim());
}
const field = (...labels) => {
  for (const label of labels) {
    for (const [key, value] of sections) {
      if (key.includes(label.toLowerCase())) return value.replace(/^_No response_$/m, "").trim();
    }
  }
  return "";
};
const checked = (label) =>
  field(label)
    .split("\n")
    .filter((line) => /^\s*-\s*\[[xX]\]/.test(line))
    .map((line) => line.replace(/^\s*-\s*\[[xX]\]\s*/, "").trim());

const display = field("provider name");
const id = field("provider id").toLowerCase();
const baseUrl = field("chat base url").replace(/\/+$/, "");
if (!display) fail("Provider name is required");
if (!/^[a-z0-9-]+$/.test(id)) fail("Provider ID must be a lowercase slug ([a-z0-9-])");
for (const [label, url] of [["Website", field("website")], ["API documentation", field("api documentation")], ["Base URL", baseUrl]]) {
  if (!/^https:\/\//.test(url)) fail(`${label} must be an https URL`);
}

function fail(message) {
  console.error(`provider-from-issue: ${message}`);
  process.exit(1);
}

const authText = field("authentication").toLowerCase();
const auth = authText.startsWith("x-api-key") ? "x-api-key" : authText.startsWith("no authentication") ? "none" : "bearer";
const dialectText = field("native protocol").toLowerCase();
const defaultDialect = dialectText.includes("responses") ? "responses" : dialectText.includes("anthropic") ? "messages" : "chat";

let extra_headers = {};
const headersRaw = field("required headers");
if (headersRaw) {
  try {
    const parsed = JSON.parse(headersRaw);
    if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) {
      extra_headers = Object.fromEntries(Object.entries(parsed).map(([k, v]) => [k, String(v)]));
    } else fail("Required headers must be a JSON object");
  } catch (error) {
    fail(`Required headers is not valid JSON: ${error.message}`);
  }
}

const optional = (label) => {
  const value = field(label);
  if (!value) return undefined;
  if (!/^https:\/\//.test(value)) fail(`${label} must be an https URL`);
  return value;
};
const maxRaw = field("default max output tokens");
const maxTokens = maxRaw ? Number.parseInt(maxRaw, 10) : undefined;
if (maxRaw && (!Number.isFinite(maxTokens) || maxTokens <= 0)) fail("Default max output tokens must be a positive integer");

const manifest = {
  id,
  display,
  website: field("website"),
  docs: field("api documentation url", "api documentation"),
  key_url: field("api-key signup", "key signup"),
  base_url: baseUrl,
  auth,
  default_dialect: defaultDialect,
  models_url: optional("models endpoint"),
  responses_url: optional("responses endpoint"),
  messages_url: optional("anthropic messages endpoint"),
  default_max_tokens: maxTokens,
  stream_only: false,
  extra_headers,
  endpoints: checked("supported endpoints"),
  capabilities: checked("model capabilities"),
  notes: field("notes"),
};
for (const key of Object.keys(manifest)) if (manifest[key] === undefined || manifest[key] === "") delete manifest[key];

const dir = path.resolve("providers/community");
fs.mkdirSync(dir, { recursive: true });
const target = path.join(dir, `${id}.json`);
const json = `${JSON.stringify(manifest, null, 2)}\n`;
if (fs.existsSync(target) && fs.readFileSync(target, "utf8") !== json) {
  fail(`${target} already exists with different content; update it in a manual PR`);
}
fs.writeFileSync(target, json);
console.log(target);
if (process.env.GITHUB_OUTPUT) {
  fs.appendFileSync(process.env.GITHUB_OUTPUT, `path=${target}\nid=${id}\n`);
}
