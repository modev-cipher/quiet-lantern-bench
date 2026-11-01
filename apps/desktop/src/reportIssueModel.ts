import type { MessageKey } from "./i18n";

export interface ReportIssueOption {
  id: "bug" | "feature" | "blank" | "website" | "security";
  titleKey: MessageKey;
  descKey: MessageKey;
  defaultTitle: string;
  defaultDesc: string;
  url: string;
}

export const BUG_REPORT_URL =
  "https://github.com/torkiv-labs/txtcaster-releases/issues/new?template=bug_report.yml";
export const FEATURE_REQUEST_URL =
  "https://github.com/torkiv-labs/txtcaster-releases/issues/new?template=feature_request.yml";
export const BLANK_ISSUE_URL =
  "https://github.com/torkiv-labs/txtcaster-releases/issues/new";
export const WEBSITE_URL =
  "https://github.com/torkiv-labs/txtcaster-releases";
export const SECURITY_URL =
  "https://github.com/torkiv-labs/txtcaster-releases/security";

export const REPORT_ISSUE_OPTIONS: readonly ReportIssueOption[] = [
  {
    id: "bug",
    titleKey: "issue.bugTitle",
    descKey: "issue.bugDesc",
    defaultTitle: "Bug report",
    defaultDesc: "Something in txtCaster is broken or behaving wrong.",
    url: BUG_REPORT_URL,
  },
  {
    id: "feature",
    titleKey: "issue.featureTitle",
    descKey: "issue.featureDesc",
    defaultTitle: "Feature request",
    defaultDesc: "Suggest an improvement or a new capability for txtCaster.",
    url: FEATURE_REQUEST_URL,
  },
  {
    id: "blank",
    titleKey: "issue.blankTitle",
    descKey: "issue.blankDesc",
    defaultTitle: "Blank issue",
    defaultDesc: "Create a new issue from scratch.",
    url: BLANK_ISSUE_URL,
  },
  {
    id: "website",
    titleKey: "issue.websiteTitle",
    descKey: "issue.websiteDesc",
    defaultTitle: "Website & product info",
    defaultDesc: "Releases, changelog, and documentation.",
    url: WEBSITE_URL,
  },
  {
    id: "security",
    titleKey: "issue.securityTitle",
    descKey: "issue.securityDesc",
    defaultTitle: "Private contact / Security",
    defaultDesc: "Report security vulnerabilities or private inquiries.",
    url: SECURITY_URL,
  },
] as const;

export function getOptionUrl(id: ReportIssueOption["id"]): string {
  const opt = REPORT_ISSUE_OPTIONS.find((o) => o.id === id);
  return opt ? opt.url : "";
}
