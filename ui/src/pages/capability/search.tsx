import { CapabilityPage } from "./shared";

/** Stored default for `POST /v1/search`. */
export default function SearchCapability() {
  return <CapabilityPage capability="search" title="Search" endpoint="POST /v1/search" />;
}
