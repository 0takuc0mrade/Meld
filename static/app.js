"use strict";

const storageKey = "meld.activeTaskId";

const elements = {
  runDemo: document.querySelector("#runDemo"),
  runLabel: document.querySelector(".primary-action__label"),
  actionHelp: document.querySelector("#actionHelp"),
  missionTitle: document.querySelector("#missionTitle"),
  missionObjective: document.querySelector("#missionObjective"),
  missionState: document.querySelector("#missionState"),
  missionStateLabel: document.querySelector("#missionStateLabel"),
  latestEvent: document.querySelector("#latestEvent"),
  connection: document.querySelector("#connection"),
  connectionLabel: document.querySelector("#connectionLabel"),
  timeline: document.querySelector("#timeline"),
  timelineEmpty: document.querySelector("#timelineEmpty"),
  sequenceBadge: document.querySelector("#sequenceBadge"),
  lifecycleRail: document.querySelector("#lifecycleRail"),
  authorityReadout: document.querySelector("#authorityReadout"),
  authorityOwner: document.querySelector("#authorityOwner"),
  authorityReason: document.querySelector("#authorityReason"),
  workerA: document.querySelector("#workerA"),
  workerB: document.querySelector("#workerB"),
  meldNode: document.querySelector("#meldNode"),
  verifier: document.querySelector("#verifier"),
  connectorA: document.querySelector("#connectorA"),
  connectorB: document.querySelector("#connectorB"),
  connectorVerifier: document.querySelector("#connectorVerifier"),
  proof: document.querySelector("#proof"),
  proofStatus: document.querySelector("#proofStatus"),
  proofList: document.querySelector("#proofList"),
  acceptedOutput: document.querySelector("#acceptedOutput"),
  acceptedComponent: document.querySelector("#acceptedComponent"),
  acceptedOnset: document.querySelector("#acceptedOnset"),
  acceptedEvidence: document.querySelector("#acceptedEvidence"),
  acceptedSummary: document.querySelector("#acceptedSummary"),
  acceptedMeta: document.querySelector("#acceptedMeta"),
  verificationProof: document.querySelector("#verificationProof"),
  verificationStatement: document.querySelector("#verificationStatement"),
  verificationChecks: document.querySelector("#verificationChecks"),
  authorityDecision: document.querySelector("#authorityDecision"),
  rejectedGeneration: document.querySelector("#rejectedGeneration"),
  acceptedGeneration: document.querySelector("#acceptedGeneration"),
  technicalDetails: document.querySelector("#technicalDetails"),
  detailTask: document.querySelector("#detailTask"),
  detailSequence: document.querySelector("#detailSequence"),
  detailStatus: document.querySelector("#detailStatus"),
  detailGeneration: document.querySelector("#detailGeneration"),
  detailWorker: document.querySelector("#detailWorker"),
  detailTransport: document.querySelector("#detailTransport"),
  detailPolicy: document.querySelector("#detailPolicy"),
  commandTrigger: document.querySelector("#commandTrigger"),
  commandPalette: document.querySelector("#commandPalette"),
  commandSearch: document.querySelector("#commandSearch"),
  commandList: document.querySelector("#commandList"),
  mainContent: document.querySelector("#mainContent"),
  siteHead: document.querySelector(".site-head"),
  siteFoot: document.querySelector(".site-foot"),
  errorToast: document.querySelector("#errorToast"),
  errorMessage: document.querySelector("#errorMessage"),
  dismissError: document.querySelector("#dismissError"),
};

const model = {
  taskId: null,
  snapshot: null,
  events: new Map(),
  eventSource: null,
  transport: "not connected",
  starting: false,
};

function setTransport(state, label) {
  model.transport = label;
  elements.connection.dataset.state = state;
  elements.connectionLabel.textContent = label;
  elements.detailTransport.textContent = label;
}

async function requestJson(url, options) {
  const response = await fetch(url, {
    ...options,
    headers: { Accept: "application/json", ...(options?.headers || {}) },
  });
  const payload = await response.json().catch(() => null);
  if (!response.ok) {
    const error = new Error(payload?.error?.message || `Request failed with status ${response.status}.`);
    error.status = response.status;
    throw error;
  }
  return payload;
}

async function startMission() {
  if (model.starting) return;
  model.starting = true;
  elements.runDemo.disabled = true;
  elements.runDemo.dataset.loading = "true";
  elements.runLabel.textContent = "Creating mission";
  elements.actionHelp.textContent = "Meld is creating a fresh authoritative task.";
  hideError();

  if (model.eventSource) {
    model.eventSource.close();
    model.eventSource = null;
  }

  try {
    const payload = await requestJson("/api/missions/demo", { method: "POST" });
    model.taskId = payload.task_id;
    model.snapshot = null;
    model.events = new Map();
    localStorage.setItem(storageKey, String(model.taskId));
    applySnapshot(payload.snapshot);
    connectEvents();
    elements.runLabel.textContent = "Run another mission";
    elements.actionHelp.textContent = `Task ${model.taskId} is controlled by the Meld supervisor.`;
  } catch (error) {
    showError(error.message);
    elements.runLabel.textContent = model.taskId ? "Run another mission" : "Run recovery mission";
    elements.actionHelp.textContent = "The server did not create a mission. Try the action again.";
  } finally {
    model.starting = false;
    elements.runDemo.disabled = false;
    delete elements.runDemo.dataset.loading;
  }
}

function applySnapshot(snapshot) {
  if (
    model.snapshot &&
    snapshot.current_sequence < model.snapshot.current_sequence
  ) {
    return;
  }
  model.snapshot = snapshot;
  model.taskId = snapshot.task_id;
  for (const event of snapshot.events) mergeEvent(event);
  render();
}

function mergeEvent(event) {
  if (!model.events.has(event.sequence)) {
    model.events.set(event.sequence, event);
  }
}

async function refreshSnapshot() {
  if (!model.taskId) return;
  try {
    const snapshot = await requestJson(`/api/tasks/${model.taskId}`);
    applySnapshot(snapshot);
  } catch (error) {
    if (error.status === 404) {
      localStorage.removeItem(storageKey);
    }
    showError(error.message);
  }
}

function connectEvents() {
  if (!model.taskId || !window.EventSource) {
    setTransport("error", "SSE unavailable");
    return;
  }
  if (model.eventSource) model.eventSource.close();

  setTransport("connecting", "Connecting to events");
  const source = new EventSource(`/api/tasks/${model.taskId}/events`);
  model.eventSource = source;

  source.addEventListener("open", () => {
    setTransport("connected", "SSE connected");
  });

  source.addEventListener("meld", (message) => {
    const event = JSON.parse(message.data);
    mergeEvent(event);
    render();
    void refreshSnapshot();
  });

  source.addEventListener("resync", () => {
    setTransport("connecting", "Resynchronizing history");
    void refreshSnapshot();
  });

  source.addEventListener("error", () => {
    if (model.eventSource === source) {
      setTransport("connecting", "Reconnecting to events");
    }
  });
}

async function restoreMission() {
  const stored = localStorage.getItem(storageKey);
  if (!stored || !/^\d+$/.test(stored)) return;
  model.taskId = Number(stored);
  try {
    await refreshSnapshot();
    if (model.snapshot) {
      elements.runLabel.textContent = "Run another mission";
      elements.actionHelp.textContent = `Task ${model.taskId} was restored from the authoritative snapshot.`;
      connectEvents();
    }
  } catch {
    localStorage.removeItem(storageKey);
  }
}

function sortedEvents() {
  return [...model.events.values()].sort((left, right) => left.sequence - right.sequence);
}

function lastEvent(kind, predicate = () => true) {
  return sortedEvents().filter((event) => event.kind === kind && predicate(event)).at(-1);
}

function hasEvent(kind, predicate = () => true) {
  return Boolean(lastEvent(kind, predicate));
}

function render() {
  const snapshot = model.snapshot;
  const events = sortedEvents();
  const latest = events.at(-1);

  if (snapshot) {
    elements.missionTitle.textContent = snapshot.mission.title;
    elements.missionObjective.textContent = snapshot.mission.objective;
    elements.missionState.dataset.state = uiState(snapshot.status.name);
    elements.missionStateLabel.textContent = snapshot.status.label;
  }

  elements.latestEvent.textContent = latest ? readableEventMessage(latest) : "No events received";
  elements.sequenceBadge.textContent = latest ? `Sequence ${latest.sequence}` : "Sequence —";
  renderTopology(snapshot, events);
  renderLifecycle(snapshot);
  renderAuthority(snapshot);
  renderTimeline(events);
  renderProof(snapshot, events);
  renderTechnical(snapshot, latest);
}

function renderLifecycle(snapshot) {
  const phases = [...elements.lifecycleRail.querySelectorAll("li")];
  const setPhase = (name, state) => {
    phases.find((phase) => phase.dataset.phase === name).dataset.state = state;
  };

  for (const phase of phases) phase.dataset.state = "idle";
  if (!snapshot) return;

  const assignedA = hasEvent("task.assigned", (event) => event.worker_id === "Worker A");
  const startedA = hasEvent("worker.started", (event) => event.worker_id === "Worker A");
  const expiredA = hasEvent("assignment.expired", (event) => event.worker_id === "Worker A");
  const reassigned = hasEvent("task.reassigned");
  const verificationStarted = hasEvent("verification.started");
  const verificationPassed = hasEvent("verification.passed");
  const staleRejected = hasEvent("submission.stale_rejected");

  if (assignedA) setPhase("assign", startedA ? "complete" : "active");
  if (startedA) setPhase("monitor", expiredA ? "complete" : "active");
  if (expiredA) setPhase("recover", reassigned ? "complete" : "active");
  if (reassigned) setPhase("verify", verificationPassed ? "complete" : verificationStarted ? "active" : "queued");
  if (verificationPassed) setPhase("defend", staleRejected ? "complete" : "watching");
}

function renderAuthority(snapshot) {
  if (!snapshot) {
    elements.authorityReadout.dataset.state = "idle";
    elements.authorityOwner.textContent = "No assignment issued";
    elements.authorityReason.textContent = "Meld will display the worker and generation currently allowed to submit.";
    return;
  }

  const status = snapshot.status;
  const accepted = snapshot.accepted_result;
  const stale = hasEvent("submission.stale_rejected");

  if (accepted) {
    elements.authorityReadout.dataset.state = "complete";
    elements.authorityOwner.textContent = `${actorLabel(accepted.worker_id)} · generation ${accepted.generation}`;
    elements.authorityReason.textContent = stale
      ? "Locked result held; the late generation was refused."
      : "Accepted result locked by deterministic policy.";
    return;
  }

  if (status.name === "recovering") {
    elements.authorityReadout.dataset.state = "recovering";
    elements.authorityOwner.textContent = `Generation ${status.generation} expired`;
    elements.authorityReason.textContent = `Meld is issuing generation ${status.next_generation}.`;
    return;
  }

  if (status.worker_id && status.generation) {
    elements.authorityReadout.dataset.state = "running";
    elements.authorityOwner.textContent = `${actorLabel(status.worker_id)} · generation ${status.generation}`;
    elements.authorityReason.textContent = "Only this assignment token may submit for verification.";
    return;
  }

  elements.authorityReadout.dataset.state = uiState(status.name);
  elements.authorityOwner.textContent = status.label;
  elements.authorityReason.textContent = "The supervisor remains the only state authority.";
}

function uiState(status) {
  if (["completed"].includes(status)) return "complete";
  if (["failed"].includes(status)) return "error";
  if (["pending", "assigned", "running", "verifying", "recovering"].includes(status)) return "running";
  return "idle";
}

function renderTopology(snapshot, events) {
  const workerAStarted = lastEvent("worker.started", (event) => event.worker_id === "Worker A");
  const agentAStarted = lastEvent("agent.execution.started", (event) => event.worker_id === "Worker A");
  const agentAParsed = lastEvent("agent.output.parsed", (event) => event.worker_id === "Worker A");
  const workerAExpired = lastEvent("assignment.expired", (event) => event.worker_id === "Worker A");
  const workerAStale = lastEvent("submission.stale_rejected", (event) => event.worker_id === "Worker A");
  const workerBStarted = lastEvent("worker.started", (event) => event.worker_id === "Worker B");
  const agentBStarted = lastEvent("agent.execution.started", (event) => event.worker_id === "Worker B");
  const agentBParsed = lastEvent("agent.output.parsed", (event) => event.worker_id === "Worker B");
  const completed = lastEvent("task.completed");
  const verificationStarted = lastEvent("verification.started");
  const verificationPassed = lastEvent("verification.passed");
  const reassigned = lastEvent("task.reassigned");

  if (workerAStale) {
    setNode(elements.workerA, "stale", "Late result rejected", workerAStale.submitted_generation, "Expired");
  } else if (workerAExpired) {
    setNode(elements.workerA, "expired", agentAParsed ? "Result arrived too late" : "Assignment timed out", workerAExpired.generation, "Revoked");
  } else if (agentAParsed) {
    setNode(elements.workerA, "running", "Candidate result ready", agentAParsed.generation, durationLabel(agentAParsed.duration_ms));
  } else if (agentAStarted) {
    setNode(elements.workerA, "running", "Analyzing evidence", agentAStarted.generation, "AI running");
  } else if (workerAStarted) {
    const lease = snapshot?.status?.worker_id === "Worker A" ? leaseLabel(snapshot.status.lease_remaining_ms) : "Backend-owned";
    setNode(elements.workerA, "running", "Executing mission", workerAStarted.generation, lease);
  } else {
    setNode(elements.workerA, "idle", "Standing by", null, "—");
  }

  if (completed) {
    setNode(elements.workerB, "complete", "Result accepted", completed.generation, "Accepted");
  } else if (agentBParsed) {
    setNode(elements.workerB, "running", "Candidate result ready", agentBParsed.generation, durationLabel(agentBParsed.duration_ms));
  } else if (agentBStarted) {
    setNode(elements.workerB, "running", "Analyzing evidence", agentBStarted.generation, "AI running");
  } else if (workerBStarted) {
    setNode(elements.workerB, "running", "Executing recovery", workerBStarted.generation, "Pending");
  } else if (workerAExpired) {
    setNode(elements.workerB, "running", "Taking ownership", reassigned?.generation, "Pending");
  } else {
    setNode(elements.workerB, "idle", "Waiting for recovery", null, "—");
  }

  if (completed) {
    setSimpleNode(elements.meldNode, "protected", workerAStale ? "Stale work refused" : "Result protected");
  } else if (snapshot?.status?.name === "recovering" || (workerAExpired && !workerBStarted)) {
    setSimpleNode(elements.meldNode, "recovering", "Reassigning authority");
  } else if (workerAStarted || workerBStarted) {
    setSimpleNode(elements.meldNode, "running", "Watching authority");
  } else {
    setSimpleNode(elements.meldNode, "idle", "Watching authority");
  }

  if (verificationPassed) {
    setSimpleNode(elements.verifier, "passed", "Policy passed");
  } else if (verificationStarted) {
    setSimpleNode(elements.verifier, "checking", "Checking result");
  } else {
    setSimpleNode(elements.verifier, "idle", "Policy ready");
  }

  elements.connectorA.dataset.state = workerAExpired ? "complete" : workerAStarted ? "active" : "idle";
  elements.connectorB.dataset.state = workerBStarted ? "complete" : workerAExpired ? "active" : "idle";
  elements.connectorVerifier.dataset.state = verificationStarted ? "complete" : workerBStarted ? "active" : "idle";
}

function leaseLabel(value) {
  if (value === null || value === undefined) return "Backend-owned";
  return `${value.toLocaleString()} ms at snapshot`;
}

function durationLabel(value) {
  if (value === null || value === undefined) return "Result ready";
  return `${(value / 1000).toFixed(1)} s`;
}

function setNode(node, state, status, generation, secondary) {
  node.dataset.state = state;
  node.querySelector(".node__status span:last-child").textContent = status;
  const values = node.querySelectorAll("dd");
  values[0].textContent = generation ? `Gen ${generation}` : "—";
  values[1].textContent = secondary;
}

function setSimpleNode(node, state, status) {
  node.dataset.state = state;
  node.querySelector(".node__status span:last-child").textContent = status;
}

function renderTimeline(events) {
  if (!events.length) {
    if (!elements.timeline.contains(elements.timelineEmpty)) {
      elements.timeline.replaceChildren(elements.timelineEmpty);
    }
    return;
  }

  const fragment = document.createDocumentFragment();
  for (const event of events) {
    const item = document.createElement("li");
    item.dataset.kind = event.kind;

    const sequence = document.createElement("span");
    sequence.className = "timeline__sequence";
    sequence.textContent = String(event.sequence).padStart(2, "0");

    const body = document.createElement("div");
    body.className = "timeline__body";
    const message = document.createElement("strong");
    message.textContent = readableEventMessage(event);
    const meta = document.createElement("span");
    meta.textContent = eventMeta(event);
    body.append(message, meta);
    item.append(sequence, body);
    fragment.append(item);
  }
  elements.timeline.replaceChildren(fragment);
}

function eventMeta(event) {
  const parts = [];
  if (event.worker_id) parts.push(actorLabel(event.worker_id));
  if (event.from_worker_id && event.to_worker_id) {
    parts.push(`${actorLabel(event.from_worker_id)} → ${actorLabel(event.to_worker_id)}`);
  }
  if (event.generation) parts.push(`generation ${event.generation}`);
  if (event.submitted_generation) parts.push(`submitted ${event.submitted_generation}`);
  if (event.current_generation) parts.push(`trusted ${event.current_generation}`);
  if (event.duration_ms !== null && event.duration_ms !== undefined) {
    parts.push(durationLabel(event.duration_ms));
  }
  const time = new Date(event.occurred_at_ms);
  if (!Number.isNaN(time.valueOf())) {
    parts.push(time.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" }));
  }
  return parts.join(" · ");
}

function actorLabel(value) {
  return value?.replace(/^Worker /, "Agent ") || value;
}

function readableEventMessage(event) {
  return event.message
    .replaceAll("Worker A", "Agent A")
    .replaceAll("Worker B", "Agent B");
}

function renderProof(snapshot) {
  const recovered = hasEvent("assignment.expired");
  const reassigned = hasEvent("task.reassigned");
  const verified = hasEvent("verification.passed") && hasEvent("task.completed");
  const stale = hasEvent("submission.stale_rejected");
  setProof("recovered", recovered);
  setProof("reassigned", reassigned);
  setProof("verified", verified);
  setProof("stale", stale);

  const accepted = snapshot?.accepted_result;
  if (!accepted) {
    elements.proof.dataset.state = "idle";
    elements.proofStatus.textContent = recovered ? "Recovery is underway; no result is authoritative yet." : "No result has been accepted.";
    elements.acceptedOutput.hidden = true;
    elements.verificationProof.hidden = true;
    elements.authorityDecision.hidden = true;
    return;
  }

  elements.proof.dataset.state = "complete";
  elements.proofStatus.textContent = stale
    ? `Generation ${accepted.generation} remains trusted after the late return.`
    : `Generation ${accepted.generation} satisfied the deterministic policy.`;
  const analysis = accepted.incident_analysis;
  elements.acceptedComponent.textContent = analysis?.affected_component || "Not provided";
  elements.acceptedOnset.textContent = formatOnset(analysis?.onset);
  elements.acceptedEvidence.textContent = analysis?.evidence_ids?.join(" · ") || accepted.evidence.join(" · ");
  elements.acceptedSummary.textContent = accepted.summary;
  elements.acceptedMeta.textContent = `${actorLabel(accepted.worker_id)} · generation ${accepted.generation}`;
  elements.acceptedOutput.hidden = false;

  elements.verificationStatement.textContent = accepted.verification.statement;
  const checks = document.createDocumentFragment();
  for (const check of accepted.verification.checks) {
    const item = document.createElement("li");
    item.dataset.passed = String(check.passed);
    const mark = document.createElement("span");
    mark.className = "proof-mark";
    mark.setAttribute("aria-hidden", "true");
    const label = document.createElement("span");
    label.textContent = check.label;
    item.append(mark, label);
    checks.append(item);
  }
  elements.verificationChecks.replaceChildren(checks);
  elements.verificationProof.hidden = false;

  const staleEvent = lastEvent("submission.stale_rejected");
  if (staleEvent) {
    elements.rejectedGeneration.textContent = `Generation ${staleEvent.submitted_generation}`;
    elements.acceptedGeneration.textContent = `Generation ${accepted.generation}`;
    elements.authorityDecision.hidden = false;
  } else {
    elements.authorityDecision.hidden = true;
  }
}

function formatOnset(value) {
  if (!value) return "Not provided";
  const date = new Date(value);
  if (Number.isNaN(date.valueOf())) return value;
  const time = date.toLocaleTimeString([], {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
    timeZone: "UTC",
    timeZoneName: "short",
  });
  const day = date.toLocaleDateString([], {
    year: "numeric",
    month: "short",
    day: "numeric",
    timeZone: "UTC",
  });
  return `${time} · ${day}`;
}

function setProof(name, confirmed) {
  const row = elements.proofList.querySelector(`[data-proof="${name}"]`);
  row.dataset.confirmed = confirmed ? "true" : "false";
}

function renderTechnical(snapshot, latest) {
  elements.detailTask.textContent = snapshot ? String(snapshot.task_id) : "—";
  elements.detailSequence.textContent = latest ? String(latest.sequence) : "—";
  elements.detailStatus.textContent = snapshot?.status?.name || "idle";
  elements.detailGeneration.textContent = snapshot?.status?.generation || snapshot?.accepted_result?.generation || "—";
  elements.detailWorker.textContent = snapshot?.status?.worker_id || snapshot?.accepted_result?.worker_id || "—";

  const policy = snapshot?.mission?.acceptance_policy;
  if (policy) {
    const terms = policy.required_terms.map((term) => `“${term}”`).join(" and ");
    elements.detailPolicy.textContent = `At least ${policy.minimum_summary_chars} characters, the terms ${terms}, and ${policy.minimum_evidence_items} evidence item. This checks structure and required content; it does not claim external factual proof.`;
  }
}

function showError(message) {
  elements.errorMessage.textContent = `${message} Check that the local server is running, then try again.`;
  elements.errorToast.hidden = false;
}

function hideError() {
  elements.errorToast.hidden = true;
}

function openCommandPalette() {
  if (elements.commandPalette.open) return;
  elements.mainContent.inert = true;
  elements.siteHead.inert = true;
  elements.siteFoot.inert = true;
  elements.commandSearch.value = "";
  filterCommands();
  elements.commandPalette.showModal();
  elements.commandSearch.focus();
}

function closeCommandPalette() {
  if (elements.commandPalette.open) elements.commandPalette.close();
}

function releaseModalBackground() {
  elements.mainContent.inert = false;
  elements.siteHead.inert = false;
  elements.siteFoot.inert = false;
  elements.commandTrigger.focus({ preventScroll: true });
}

function visibleCommands() {
  return [...elements.commandList.querySelectorAll("button:not([hidden])")];
}

function selectedCommand() {
  return visibleCommands().find((button) => button.getAttribute("aria-selected") === "true");
}

function selectCommand(button) {
  for (const command of visibleCommands()) {
    command.setAttribute("aria-selected", command === button ? "true" : "false");
  }
  button.scrollIntoView({ block: "nearest" });
}

function moveCommand(offset) {
  const commands = visibleCommands();
  if (!commands.length) return;
  const current = Math.max(0, commands.indexOf(selectedCommand()));
  selectCommand(commands[(current + offset + commands.length) % commands.length]);
}

function filterCommands() {
  const query = elements.commandSearch.value.trim().toLowerCase();
  const commands = [...elements.commandList.querySelectorAll("button")];
  for (const command of commands) {
    command.hidden = !command.textContent.toLowerCase().includes(query);
    command.setAttribute("aria-selected", "false");
  }
  const first = visibleCommands()[0];
  if (first) first.setAttribute("aria-selected", "true");
}

function executeCommand(command) {
  closeCommandPalette();
  if (command === "run") void startMission();
  if (command === "events") document.querySelector("#event-ledger").scrollIntoView({ block: "start" });
  if (command === "details") {
    elements.technicalDetails.open = !elements.technicalDetails.open;
    elements.technicalDetails.scrollIntoView({ block: "start" });
  }
}

elements.runDemo.addEventListener("click", () => void startMission());
elements.dismissError.addEventListener("click", hideError);
elements.commandTrigger.addEventListener("click", openCommandPalette);
elements.commandPalette.addEventListener("close", releaseModalBackground);
elements.commandPalette.addEventListener("cancel", (event) => {
  event.preventDefault();
  closeCommandPalette();
});
elements.commandPalette.addEventListener("click", (event) => {
  if (event.target === elements.commandPalette) closeCommandPalette();
});
elements.commandSearch.addEventListener("input", filterCommands);
elements.commandSearch.addEventListener("keydown", (event) => {
  if (event.key === "ArrowDown") {
    event.preventDefault();
    moveCommand(1);
  }
  if (event.key === "ArrowUp") {
    event.preventDefault();
    moveCommand(-1);
  }
  if (event.key === "Enter") {
    event.preventDefault();
    const selected = selectedCommand();
    if (selected) executeCommand(selected.dataset.command);
  }
});
elements.commandList.addEventListener("click", (event) => {
  const button = event.target.closest("button[data-command]");
  if (button) executeCommand(button.dataset.command);
});
document.addEventListener("keydown", (event) => {
  if (event.key === "Escape" && elements.commandPalette.open) {
    event.preventDefault();
    closeCommandPalette();
    return;
  }
  if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "k") {
    event.preventDefault();
    elements.commandPalette.open ? closeCommandPalette() : openCommandPalette();
  }
});
window.addEventListener("beforeunload", () => model.eventSource?.close());

if (!/Mac|iPhone|iPad/.test(navigator.platform)) {
  elements.commandTrigger.querySelector("kbd").textContent = "Ctrl K";
}

void restoreMission();
