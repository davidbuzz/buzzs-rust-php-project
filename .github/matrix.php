<?php
// Which CI jobs run, and with which variations: php-src's .github/matrix.php for this
// repository (see CI_PLAN.md). The same arguments, labels and outputs; the job list is
// ours: no release branches (only the branch under test), no ZTS/ASAN axes, no Windows,
// macOS, FreeBSD, Solaris, PPC64, X32, MSAN or libmysqlclient jobs, plus LINUX_ARM64 and
// LATEST_STABLE_RUST.
//
// usage: php .github/matrix.php <event> <attempt> <ref> <labels-json> <repository>

function select_jobs(string $trigger, bool $nightly, array $labels, bool $all_variations): array {
    $no_jobs = in_array('CI: No jobs', $labels, true);
    $all_jobs = in_array('CI: All jobs', $labels, true) || $nightly;
    $test_benchmarking = in_array('CI: Benchmarking', $labels, true);
    $test_community = in_array('CI: Community', $labels, true);
    $test_latest_stable = in_array('CI: Latest Stable Rust', $labels, true);
    $test_linux_arm64 = in_array('CI: Linux ARM64', $labels, true);
    $test_linux_x64 = in_array('CI: Linux X64', $labels, true);
    $test_opcache_variation = in_array('CI: Opcache Variation', $labels, true);

    $jobs = [];
    // Upstream: pull requests, and pushes to php/php-src, which store their numbers in
    // php/benchmarking-data. We have no such repository, so pull requests only, compared
    // with their base commit.
    if (!$nightly && $trigger === 'pull_request' && ($all_jobs || !$no_jobs || $test_benchmarking)) {
        $jobs['BENCHMARKING'] = true;
    }
    if ($all_jobs || $test_community) {
        $jobs['COMMUNITY'] = true;
    }
    if ($all_jobs || !$no_jobs || $test_linux_x64) {
        $jobs['LINUX_X64']['matrix'] = $all_variations
            ? [
                'name' => [''],
                'debug' => [true, false],
                'repeat' => [false],
                'variation' => [false],
                'include' => [
                    ['name' => '_REPEAT', 'debug' => true, 'repeat' => true, 'variation' => false],
                    ['name' => '_VARIATION', 'debug' => true, 'repeat' => false, 'variation' => true],
                ],
            ]
            : ['include' => [
                ['name' => '', 'debug' => false, 'repeat' => false, 'variation' => false],
            ]];
    }
    // Ours alone: the AArch64 build (the trace JIT's AArch64 backend, the fibers there).
    if ($all_jobs || !$no_jobs || $test_linux_arm64) {
        $jobs['LINUX_ARM64']['matrix'] = $all_variations
            ? ['debug' => [true, false]]
            : ['include' => [['debug' => false]]];
    }
    if ($all_jobs || $test_opcache_variation) {
        $jobs['OPCACHE_VARIATION'] = true;
    }
    // Ours alone: the jobs above use the pinned compiler; this one the latest stable, so a
    // compiler change shows up in a run of its own (rustc 1.100 changed Value's layout on
    // AArch64, 24 Nov 2026).
    if ($nightly || $test_latest_stable) {
        $jobs['LATEST_STABLE_RUST'] = true;
    }
    return $jobs;
}

$trigger = $argv[1] ?? 'schedule';
$attempt = (int) ($argv[2] ?? 1);
$ref = $argv[3] ?? 'master';
$labels = json_decode($argv[4] ?? '[]', true) ?? [];
$labels = array_column($labels, 'name');
$repository = $argv[5] ?? null;

$nightly = $trigger === 'schedule' || $trigger === 'workflow_dispatch';
$all_variations = $nightly || in_array('CI: All variations', $labels, true);

$branches = [[
    'name' => 'Suite',
    'ref' => $ref,
    'jobs' => select_jobs($trigger, $nightly, $labels, $all_variations),
    'config' => [
        'ubuntu_version' => '24.04',
        // The compiler this repository builds with (the development box's); the job
        // LATEST_STABLE_RUST uses `stable` instead.
        'rust_toolchain' => '1.95.0',
    ],
]];

echo "All variations:";
echo json_encode($all_variations, JSON_UNESCAPED_SLASHES|JSON_PRETTY_PRINT);
echo "\n\nBranches:\n";
echo json_encode($branches, JSON_UNESCAPED_SLASHES|JSON_PRETTY_PRINT);
echo "\n";

if (false !== ($github_output = getenv('GITHUB_OUTPUT'))) {
    $f = fopen($github_output, 'a');
    fwrite($f, 'branches=' . json_encode($branches, JSON_UNESCAPED_SLASHES) . "\n");
    fwrite($f, 'all_variations=' . json_encode($all_variations, JSON_UNESCAPED_SLASHES) . "\n");
    fclose($f);
}
