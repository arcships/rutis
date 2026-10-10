#!/usr/bin/env node
// CI measurements (Q12.9) for the last N completed runs of a workflow:
// push-to-result time (including queueing), queue time and duration per job,
// failures and cancellations, failures on main, reruns. Prints Markdown.
//
//   node tools/ci-stats.mjs [N=30] [workflow=ci.yml]
//
// Needs the GitHub CLI (`gh`), authenticated; set GH_REPO outside a checkout.

import { execFileSync } from 'node:child_process';

const count = Number(process.argv[2] ?? 30);
const workflow = process.argv[3] ?? 'ci.yml';

const api = (path) => JSON.parse(execFileSync('gh', ['api', path], { encoding: 'utf8', maxBuffer: 1 << 28 }));
const minutes = (from, to) => Math.max(0, (Date.parse(to) - Date.parse(from)) / 60000);
const fmt = (value) => (value === undefined || Number.isNaN(value) ? '—' : value.toFixed(1));
const quantile = (values, q) => {
  if (values.length === 0) return undefined;
  const sorted = [...values].sort((a, b) => a - b);
  return sorted[Math.min(sorted.length - 1, Math.floor(q * sorted.length))];
};

const runs = api(`repos/{owner}/{repo}/actions/workflows/${workflow}/runs?status=completed&per_page=${Math.min(count, 100)}`)
  .workflow_runs.slice(0, count);

const rows = [];
const jobs = new Map();
for (const run of runs) {
  const runJobs = api(`repos/{owner}/{repo}/actions/runs/${run.id}/attempts/${run.run_attempt}/jobs?per_page=100`).jobs;
  const ran = runJobs.filter((job) => job.started_at && job.completed_at && job.conclusion !== 'skipped');
  const end = ran.reduce((last, job) => (job.completed_at > last ? job.completed_at : last), run.created_at);
  rows.push({
    run,
    total: minutes(run.created_at, end),
    queue: Math.max(0, ...ran.map((job) => minutes(job.created_at, job.started_at))),
    jobs: ran.length,
  });
  for (const job of runJobs) {
    // Matrix jobs keep their matrix suffix: each one takes its own runner.
    const entry = jobs.get(job.name) ?? { durations: [], queues: [], failure: 0, cancelled: 0, skipped: 0 };
    if (job.conclusion === 'skipped') entry.skipped += 1;
    // Times only from runs that finished; a superseded run's jobs stop early.
    else if (run.conclusion !== 'cancelled' && job.started_at && job.completed_at) {
      entry.durations.push(minutes(job.started_at, job.completed_at));
      entry.queues.push(minutes(job.created_at, job.started_at));
    }
    if (job.conclusion === 'failure') entry.failure += 1;
    if (job.conclusion === 'cancelled') entry.cancelled += 1;
    jobs.set(job.name, entry);
  }
}

const out = [];
const since = runs.length ? runs[runs.length - 1].created_at.slice(0, 10) : '—';
out.push(`## CI measurements: \`${workflow}\`, last ${runs.length} completed runs (since ${since})`, '');

out.push('### Push to result (minutes, including queueing)', '');
out.push('| Event | Runs | Median | p90 | Max | Failed | Cancelled |', '| --- | ---: | ---: | ---: | ---: | ---: | ---: |');
for (const event of [...new Set(runs.map((run) => run.event))]) {
  const group = rows.filter((row) => row.run.event === event);
  const totals = group.filter((row) => row.run.conclusion !== 'cancelled').map((row) => row.total);
  const failed = group.filter((row) => row.run.conclusion === 'failure').length;
  const cancelled = group.filter((row) => row.run.conclusion === 'cancelled').length;
  out.push(`| ${event} | ${group.length} | ${fmt(quantile(totals, 0.5))} | ${fmt(quantile(totals, 0.9))} | ${fmt(quantile(totals, 1))} | ${failed} | ${cancelled} |`);
}
out.push('', 'Cancelled runs (superseded by a newer push) are left out of all times below too.', '');

out.push('### Jobs (minutes)', '');
out.push('| Job | Ran | Skipped | Duration median | Duration p90 | Queue median | Queue max | Failed | Cancelled |');
out.push('| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |');
const byDuration = [...jobs].sort((a, b) => (quantile(b[1].durations, 0.5) ?? 0) - (quantile(a[1].durations, 0.5) ?? 0));
for (const [name, job] of byDuration) {
  out.push(`| ${name} | ${job.durations.length} | ${job.skipped} | ${fmt(quantile(job.durations, 0.5))} | ${fmt(quantile(job.durations, 0.9))} | ${fmt(quantile(job.queues, 0.5))} | ${fmt(quantile(job.queues, 1))} | ${job.failure} | ${job.cancelled} |`);
}
out.push('');

const mainFailures = rows.filter((row) => row.run.event === 'push' && row.run.head_branch === 'main' && row.run.conclusion === 'failure');
out.push(`### Failures on main: ${mainFailures.length}`, '');
for (const { run } of mainFailures) out.push(`- [${run.head_sha.slice(0, 7)}](${run.html_url}) ${run.created_at} ${run.display_title}`);
const reruns = rows.filter((row) => row.run.run_attempt > 1);
out.push('', `### Rerun runs (possible flaky failures): ${reruns.length}`, '');
for (const { run } of reruns) out.push(`- [${run.id}](${run.html_url}) attempt ${run.run_attempt}, ${run.conclusion}: ${run.display_title}`);
out.push('', '### Runs', '');
out.push('| Run | Event | Branch | Result | Push to result | Longest queue | Jobs run |', '| --- | --- | --- | --- | ---: | ---: | ---: |');
for (const { run, total, queue, jobs: ran } of rows) {
  out.push(`| [${run.id}](${run.html_url}) | ${run.event} | ${run.head_branch} | ${run.conclusion} | ${fmt(total)} | ${fmt(queue)} | ${ran} |`);
}
console.log(out.join('\n'));
