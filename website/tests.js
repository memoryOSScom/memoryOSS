function createSummary(report) {
  const summary = document.createElement("div");
  summary.className = "summary-bar";
  const metrics = [
    { num: report.summary.total_checks_passed, label: "Passed Checks", color: "var(--green)" },
    { num: report.summary.sections, label: "Sections" },
    { num: report.summary.rust_unit_tests, label: "Rust Unit" },
    { num: report.summary.rust_integration_tests, label: "Rust Integration" },
    { num: report.summary.typescript_tests, label: "TypeScript" },
    { num: report.summary.wizard_assertions, label: "Wizard Assertions" },
    { num: report.summary.wizard_scenarios || "-", label: "Wizard Scenarios" },
    { num: report.summary.benchmark_memories ? report.summary.benchmark_memories.toLocaleString() : "-", label: "Benchmark Memories" },
    { num: report.summary.calibration_queries ? report.summary.calibration_queries.toLocaleString() : "-", label: "Calibration Queries" },
    { num: report.duration_seconds + "s", label: "Duration" },
  ];

  for (const metric of metrics) {
    const item = document.createElement("div");
    item.className = "summary-item";
    const num = document.createElement("div");
    num.className = "num";
    if (metric.color) num.style.color = metric.color;
    num.textContent = metric.num;
    const label = document.createElement("div");
    label.className = "label";
    label.textContent = metric.label;
    item.appendChild(num);
    item.appendChild(label);
    summary.appendChild(item);
  }
  return summary;
}

function createCard(title, count, items, description) {
  const card = document.createElement("div");
  card.className = "card";

  const heading = document.createElement("h2");
  heading.textContent = count ? `${title} (${count})` : title;
  card.appendChild(heading);

  if (description) {
    const desc = document.createElement("p");
    desc.textContent = description;
    card.appendChild(desc);
  }

  for (const entry of items) {
    const row = document.createElement("div");
    row.className = "test-row";

    const status = document.createElement("span");
    status.className = `status ${entry.status}`;
    status.textContent = entry.status.toUpperCase();

    const body = document.createElement("div");
    const name = document.createElement("div");
    name.textContent = entry.name;
    body.appendChild(name);

    if (entry.note) {
      const note = document.createElement("div");
      note.className = "note";
      note.textContent = entry.note;
      body.appendChild(note);
    }

    row.appendChild(status);
    row.appendChild(body);
    card.appendChild(row);
  }

  return card;
}

async function loadReport() {
  try {
    const response = await fetch("tests-report.json", { cache: "no-store" });
    if (!response.ok) {
      throw new Error(`HTTP ${response.status}`);
    }
    const report = await response.json();

    const summaryRoot = document.getElementById("summary");
    summaryRoot.appendChild(createSummary(report));

    const sectionsRoot = document.getElementById("sections");
    for (const section of report.sections) {
      sectionsRoot.appendChild(createCard(section.title, section.count, section.items));
    }

    if (report.coverage_gaps && report.coverage_gaps.length > 0) {
      sectionsRoot.appendChild(
        createCard(
          "Coverage Gaps",
          report.coverage_gaps.length,
          report.coverage_gaps.map((gap) => ({ name: gap, status: "skip" })),
          "These paths are known and documented, but still outside the default automated runner."
        )
      );
    }

    const meta = document.getElementById("meta");
    meta.className = "meta";
    const generatedAt = new Date(report.generated_at);
    meta.textContent = `Last verified: ${generatedAt.toLocaleString("en-GB", {
      dateStyle: "long",
      timeStyle: "short",
      timeZone: "UTC",
    })} UTC - ${report.runner}`;
  } catch (error) {
    const summaryRoot = document.getElementById("summary");
    const errorCard = document.createElement("div");
    errorCard.className = "card error";
    const title = document.createElement("h2");
    title.textContent = "Report Unavailable";
    const text = document.createElement("p");
    text.textContent = `Could not load tests-report.json: ${error.message}`;
    errorCard.appendChild(title);
    errorCard.appendChild(text);
    summaryRoot.appendChild(errorCard);
  }
}

loadReport();
