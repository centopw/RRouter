import { CapabilityPage } from "./shared";

/** Stored default for `POST /v1/embeddings`. */
export default function EmbeddingsCapability() {
  return (
    <CapabilityPage
      capability="embeddings"
      title="Embeddings"
      endpoint="POST /v1/embeddings"
      providerWording="eligible"
      eligibilityNote="Eligible providers are OpenAI-dialect; the upstream must expose /embeddings."
    />
  );
}
