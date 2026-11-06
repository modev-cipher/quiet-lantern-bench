import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import ReportIssueModal from "./ReportIssueModal";
import {
  BLANK_ISSUE_URL,
  BUG_REPORT_URL,
  EMAIL_SUPPORT_URL,
  FEATURE_REQUEST_URL,
  getOptionUrl,
  REPORT_ISSUE_OPTIONS,
  SECURITY_URL,
  SUPPORT_URL,
  WEBSITE_URL,
} from "./reportIssueModel";
import { CATALOGS } from "./i18n/catalog";

describe("reportIssueModel options and URLs", () => {
  it("defines the issue, contact and support destinations", () => {
    expect(REPORT_ISSUE_OPTIONS).toHaveLength(7);
    const ids = REPORT_ISSUE_OPTIONS.map((o) => o.id);
    expect(ids).toEqual([
      "bug",
      "feature",
      "email",
      "blank",
      "website",
      "security",
      "support",
    ]);
  });

  it("points each option to the correct official destination URL", () => {
    expect(getOptionUrl("bug")).toBe(BUG_REPORT_URL);
    expect(BUG_REPORT_URL).toBe(
      "https://github.com/torkiv-labs/txtcaster-releases/issues/new?template=bug_report.yml",
    );

    expect(getOptionUrl("feature")).toBe(FEATURE_REQUEST_URL);
    expect(FEATURE_REQUEST_URL).toBe(
      "https://github.com/torkiv-labs/txtcaster-releases/issues/new?template=feature_request.yml",
    );

    expect(getOptionUrl("blank")).toBe(BLANK_ISSUE_URL);
    expect(BLANK_ISSUE_URL).toBe(
      "https://github.com/torkiv-labs/txtcaster-releases/issues/new",
    );

    expect(getOptionUrl("website")).toBe(WEBSITE_URL);
    expect(WEBSITE_URL).toBe(
      "https://github.com/torkiv-labs/txtcaster-releases",
    );

    expect(getOptionUrl("security")).toBe(SECURITY_URL);
    expect(SECURITY_URL).toBe(
      "https://github.com/torkiv-labs/txtcaster-releases/security",
    );

    expect(getOptionUrl("email")).toBe(EMAIL_SUPPORT_URL);
    expect(EMAIL_SUPPORT_URL).toBe(
      "mailto:support@txtcaster.torkiv.com?subject=txtCaster%20support",
    );

    expect(getOptionUrl("support")).toBe(SUPPORT_URL);
    expect(SUPPORT_URL).toBe("https://www.patreon.com/c/txtcaster/membership");
  });
});

describe("ReportIssueModal markup and accessibility", () => {
  it("renders nothing when open is false", () => {
    const html = renderToStaticMarkup(
      createElement(ReportIssueModal, {
        open: false,
        onClose: () => {},
      }),
    );
    expect(html).toBe("");
  });

  it("renders dialog surface with accessible attributes and every option when open", () => {
    const html = renderToStaticMarkup(
      createElement(ReportIssueModal, {
        open: true,
        onClose: () => {},
      }),
    );

    expect(html).toContain('data-testid="report-issue-modal"');
    expect(html).toContain('role="dialog"');
    expect(html).toContain('aria-modal="true"');
    expect(html).toContain('data-testid="report-issue-close"');

    expect(html).toContain('data-testid="issue-option-bug"');
    expect(html).toContain('data-testid="issue-option-feature"');
    expect(html).toContain('data-testid="issue-option-blank"');
    expect(html).toContain('data-testid="issue-option-website"');
    expect(html).toContain('data-testid="issue-option-security"');
    expect(html).toContain('data-testid="issue-option-email"');
    expect(html).toContain('data-testid="issue-option-support"');

    // English copy rendered
    expect(html).toContain("Report an Issue / Feedback");
    expect(html).toContain("Bug report");
    expect(html).toContain("Feature request");
    expect(html).toContain("Blank issue");
    expect(html).toContain("Website &amp; product info");
    expect(html).toContain("Private contact / Security");
    expect(html).toContain("Email support");
    expect(html).toContain("support@txtcaster.torkiv.com");
    expect(html).toContain("Support development");
  });
});

describe("i18n catalog parity for issue reporting", () => {
  it("contains all issue and feedback keys in both English and Arabic", () => {
    const requiredKeys = [
      "nav.reportIssue",
      "nav.reportIssueTitle",
      "nav.feedback",
      "issue.modalTitle",
      "issue.modalSubtitle",
      "issue.close",
      "issue.bugTitle",
      "issue.bugDesc",
      "issue.featureTitle",
      "issue.featureDesc",
      "issue.blankTitle",
      "issue.blankDesc",
      "issue.websiteTitle",
      "issue.websiteDesc",
      "issue.securityTitle",
      "issue.securityDesc",
      "issue.emailTitle",
      "issue.emailDesc",
      "issue.supportTitle",
      "issue.supportDesc",
      "settings.reportIssue",
      "settings.reportIssueHint",
      "settings.openReportIssue",
    ];

    for (const key of requiredKeys) {
      expect(CATALOGS.en[key as keyof typeof CATALOGS.en]).toBeTruthy();
      expect(CATALOGS.ar[key as keyof typeof CATALOGS.ar]).toBeTruthy();
    }
  });
});
