import { CapabilityPage } from "./shared";

/** Stored default for `POST /v1/rerank`. */
export default function RerankCapability() {
  return <CapabilityPage capability="rerank" title="Rerank" endpoint="POST /v1/rerank" />;
}
