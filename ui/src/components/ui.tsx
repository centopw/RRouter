/** Unified UI primitive barrel (all 27 atoms and 13 molecules). */
export { cx } from "../lib/cx";

// Atoms (27 modules)
export { Badge, type BadgeSize, type BadgeTone } from "./atoms/Badge";
export { Button } from "./atoms/Button";
export { Checkbox } from "./atoms/Checkbox";
export { CopyButton } from "./atoms/CopyButton";
export { CopyChip } from "./atoms/CopyChip";
export { CopyIconButton } from "./atoms/CopyIconButton";
export { Divider } from "./atoms/Divider";
export { Dot, type DotTone } from "./atoms/Dot";
export { FilterChip } from "./atoms/FilterChip";
export { Grid } from "./atoms/Grid";
export { Heading } from "./atoms/Heading";
export { IconButton } from "./atoms/IconButton";
export { Inline } from "./atoms/Inline";
export { control, Input, type ControlSize } from "./atoms/Input";
export { Meter, barFill, barText } from "./atoms/Meter";
export { SectionLabel } from "./atoms/SectionLabel";
export { Segmented } from "./atoms/Segmented";
export { Select } from "./atoms/Select";
export { Stack, type StackProps } from "./atoms/Stack";
export { statusText, StatusPill } from "./atoms/StatusPill";
export { TabPanel, TabStrip } from "./atoms/TabStrip";
export { Text } from "./atoms/Text";
export { Textarea } from "./atoms/Textarea";
export { Toggle } from "./atoms/Toggle";
export { useCopy } from "./atoms/useCopy";
export type { Gap } from "./atoms/layout";
export type { TextVariant, Tone } from "./atoms/typography";

// Molecules (13 modules)
export { AlertBanner, type AlertTone } from "./molecules/AlertBanner";
export { Card } from "./molecules/Card";
export { CardFooter } from "./molecules/CardFooter";
export { CardHeader, type CardHeaderPadding } from "./molecules/CardHeader";
export { ConfirmDialog } from "./molecules/ConfirmDialog";
export { Disclosure } from "./molecules/Disclosure";
export { EmptyState } from "./molecules/EmptyState";
export { ErrorNote } from "./molecules/ErrorNote";
export { Field } from "./molecules/Field";
export { Modal, type ModalWidth } from "./molecules/Modal";
export { PageHeader } from "./molecules/PageHeader";
export { PageLayout, type PageDensity, type PageLayoutVariant } from "./molecules/PageLayout";
export { Tabs } from "./molecules/Tabs";
