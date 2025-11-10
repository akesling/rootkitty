use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use rootkitty::scanner::{Scanner, ScannerImpl};
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

/// Create a deterministic directory structure for benchmarking
///
/// Structure created:
/// - Root directory
///   - 5 subdirectories at depth 1
///     - Each contains 10 subdirectories at depth 2
///       - Each contains 20 files (~100 bytes each)
///       - Each contains 3 subdirectories at depth 3
///         - Each contains 10 files (~100 bytes each)
///
/// Total: ~5 + 50 + 1000 files + 150 + 1500 files = ~2,705 filesystem entries
fn create_benchmark_tree(root: &PathBuf, breadth_1: usize, breadth_2: usize, files_per_dir: usize) {
    // Create root
    fs::create_dir_all(root).unwrap();

    // Level 1: Create main subdirectories
    for i in 0..breadth_1 {
        let dir1 = root.join(format!("dir1_{:03}", i));
        fs::create_dir_all(&dir1).unwrap();

        // Level 2: Create subdirectories
        for j in 0..breadth_2 {
            let dir2 = dir1.join(format!("dir2_{:03}", j));
            fs::create_dir_all(&dir2).unwrap();

            // Create files at level 2
            for k in 0..files_per_dir {
                let file = dir2.join(format!("file_{:03}.txt", k));
                fs::write(&file, "x".repeat(100)).unwrap();
            }

            // Level 3: Create deeper subdirectories (fewer, to keep it manageable)
            for l in 0..3 {
                let dir3 = dir2.join(format!("dir3_{:03}", l));
                fs::create_dir_all(&dir3).unwrap();

                // Create files at level 3
                for m in 0..10 {
                    let file = dir3.join(format!("file_{:03}.txt", m));
                    fs::write(&file, "y".repeat(100)).unwrap();
                }
            }
        }
    }
}

/// Benchmark scanning with different directory structures
fn bench_scanner_directory_walk(c: &mut Criterion) {
    let mut group = c.benchmark_group("directory_walk");

    // Small tree: 5 dirs, 10 subdirs each, 20 files each = ~1,300 entries
    let small_tree = TempDir::new().unwrap();
    create_benchmark_tree(&small_tree.path().to_path_buf(), 5, 10, 20);

    group.bench_with_input(
        BenchmarkId::new("small_tree", "5x10x20"),
        &small_tree.path(),
        |b, path| {
            b.iter(|| {
                let scanner = Scanner::new(black_box(path));
                scanner.scan().unwrap()
            })
        },
    );

    // Medium tree: 10 dirs, 15 subdirs each, 30 files each = ~5,260 entries
    let medium_tree = TempDir::new().unwrap();
    create_benchmark_tree(&medium_tree.path().to_path_buf(), 10, 15, 30);

    group.bench_with_input(
        BenchmarkId::new("medium_tree", "10x15x30"),
        &medium_tree.path(),
        |b, path| {
            b.iter(|| {
                let scanner = Scanner::new(black_box(path));
                scanner.scan().unwrap()
            })
        },
    );

    // Large tree: 15 dirs, 20 subdirs each, 40 files each = ~12,915 entries
    let large_tree = TempDir::new().unwrap();
    create_benchmark_tree(&large_tree.path().to_path_buf(), 15, 20, 40);

    group.bench_with_input(
        BenchmarkId::new("large_tree", "15x20x40"),
        &large_tree.path(),
        |b, path| {
            b.iter(|| {
                let scanner = Scanner::new(black_box(path));
                scanner.scan().unwrap()
            })
        },
    );

    group.finish();
}

/// Benchmark scanning with different depths (narrow but deep trees)
fn bench_scanner_depth(c: &mut Criterion) {
    let mut group = c.benchmark_group("directory_depth");

    for depth in [5, 10, 15] {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path().to_path_buf();

        // Create a narrow but deep tree
        let mut current = root.clone();
        for i in 0..depth {
            current = current.join(format!("level_{}", i));
            fs::create_dir_all(&current).unwrap();

            // Add a few files at each level
            for j in 0..5 {
                let file = current.join(format!("file_{}.txt", j));
                fs::write(&file, "z".repeat(100)).unwrap();
            }
        }

        group.bench_with_input(BenchmarkId::new("depth", depth), &root, |b, path| {
            b.iter(|| {
                let scanner = Scanner::new(black_box(path));
                scanner.scan().unwrap()
            })
        });
    }

    group.finish();
}

/// Comparative benchmark: Custom implementation vs walkdir vs hybrid
fn bench_scanner_comparison(c: &mut Criterion) {
    let mut group = c.benchmark_group("comparison");

    // Create a medium-sized test tree for comparison
    let test_tree = TempDir::new().unwrap();
    create_benchmark_tree(&test_tree.path().to_path_buf(), 10, 15, 30);
    let test_path = test_tree.path();

    // Benchmark custom implementation (rayon-based parallel)
    group.bench_with_input(
        BenchmarkId::new("custom_impl", "10x15x30"),
        &test_path,
        |b, path| {
            b.iter(|| {
                let scanner = Scanner::new_with_impl(black_box(path), ScannerImpl::Custom);
                scanner.scan().unwrap()
            })
        },
    );

    // Benchmark walkdir implementation (single-threaded)
    group.bench_with_input(
        BenchmarkId::new("walkdir_impl", "10x15x30"),
        &test_path,
        |b, path| {
            b.iter(|| {
                let scanner = Scanner::new_with_impl(black_box(path), ScannerImpl::Walkdir);
                scanner.scan().unwrap()
            })
        },
    );

    // Benchmark hybrid implementation (walkdir + rayon)
    group.bench_with_input(
        BenchmarkId::new("hybrid_impl", "10x15x30"),
        &test_path,
        |b, path| {
            b.iter(|| {
                let scanner = Scanner::new_with_impl(black_box(path), ScannerImpl::Hybrid);
                scanner.scan().unwrap()
            })
        },
    );

    group.finish();
}

/// Benchmark different tree sizes with all three implementations
fn bench_scanner_scaling(c: &mut Criterion) {
    let mut group = c.benchmark_group("scaling");

    let configs = [
        ("small", 5, 10, 20),
        ("medium", 10, 15, 30),
        ("large", 15, 20, 40),
    ];

    for (size_name, breadth_1, breadth_2, files) in configs {
        let test_tree = TempDir::new().unwrap();
        create_benchmark_tree(&test_tree.path().to_path_buf(), breadth_1, breadth_2, files);
        let test_path = test_tree.path();

        // Custom implementation
        group.bench_with_input(
            BenchmarkId::new("custom", size_name),
            &test_path,
            |b, path| {
                b.iter(|| {
                    let scanner = Scanner::new_with_impl(black_box(path), ScannerImpl::Custom);
                    scanner.scan().unwrap()
                })
            },
        );

        // Walkdir implementation
        group.bench_with_input(
            BenchmarkId::new("walkdir", size_name),
            &test_path,
            |b, path| {
                b.iter(|| {
                    let scanner = Scanner::new_with_impl(black_box(path), ScannerImpl::Walkdir);
                    scanner.scan().unwrap()
                })
            },
        );

        // Hybrid implementation
        group.bench_with_input(
            BenchmarkId::new("hybrid", size_name),
            &test_path,
            |b, path| {
                b.iter(|| {
                    let scanner = Scanner::new_with_impl(black_box(path), ScannerImpl::Hybrid);
                    scanner.scan().unwrap()
                })
            },
        );
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_scanner_directory_walk,
    bench_scanner_depth,
    bench_scanner_comparison,
    bench_scanner_scaling
);
criterion_main!(benches);
