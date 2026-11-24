// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! End-to-end tests: PHP scripts run on a VM with the `gd` extension (plus
//! the stream and string functions they use). Expected outputs follow
//! `/usr/bin/php` (8.3 against the same system libgd) with php-src master
//! messages.

use std::cell::RefCell;
use std::io::Write;
use std::path::PathBuf;
use std::rc::Rc;

use php_vm::ext::Extension;
use php_vm::{Vm, VmOptions};

/// A `Write` sink shared with the test.
#[derive(Clone, Default)]
struct Sink(Rc<RefCell<Vec<u8>>>);

impl Write for Sink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Keeps the test process's standard descriptors: `php-ext-streams`
/// really closes them when the VM's `STDIN`/`STDOUT`/`STDERR` resources
/// are released, which would silence the test harness.
struct SavedFds([i32; 3]);

impl SavedFds {
    fn save() -> Self {
        // SAFETY: duplicating the standard descriptors has no memory effects.
        SavedFds([0, 1, 2].map(|fd| unsafe { libc::dup(fd) }))
    }
}

impl Drop for SavedFds {
    fn drop(&mut self) {
        for (fd, saved) in (0..).zip(self.0) {
            if saved >= 0 {
                // SAFETY: `saved` is a descriptor we own; it is moved back
                // onto the standard slot and then closed.
                unsafe {
                    libc::dup2(saved, fd);
                    libc::close(saved);
                }
            }
        }
    }
}

/// A fresh directory for one test.
fn tmpdir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("php-ext-gd-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("temp dir");
    d
}

/// Runs `code` (without `<?php`, `$dir` set to a scratch directory) and
/// returns its output.
fn run(name: &str, code: &str) -> String {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let _fds = SavedFds::save();
    let dir = tmpdir(name);
    let sink = Sink::default();
    let streams = php_ext_streams::Streams;
    let stdstr = php_ext_stdstr::StdStr;
    let gd = php_ext_gd::Gd;
    let exts: [&dyn Extension; 3] = [&stdstr, &streams, &gd];
    let opts = VmOptions {
        ini: vec![
            ("display_errors".into(), "1".into()),
            ("html_errors".into(), "0".into()),
        ],
    };
    let mut vm = Vm::with_extensions(&opts, Box::new(sink.clone()), &exts);
    let src = format!("<?php $dir = '{}';\n{code}", dir.display());
    vm.run_script(src.as_bytes(), "test.php");
    drop(vm);
    let _ = std::fs::remove_dir_all(&dir);
    let out = sink.0.borrow().clone();
    String::from_utf8_lossy(&out).into_owned()
}

#[test]
fn draw_save_and_reload() {
    let out = run(
        "reload",
        r#"$im = imagecreatetruecolor(10, 8);
        $red = imagecolorallocate($im, 255, 0, 0);
        imagefilledrectangle($im, 1, 1, 4, 3, $red);
        var_dump($red, imagecolorat($im, 2, 2), imagecolorat($im, 9, 7), imagesx($im), imagesy($im));
        echo implode(',', imagecolorsforindex($im, $red)), "\n";
        var_dump(imagepng($im, "$dir/a.png"));
        $re = imagecreatefrompng("$dir/a.png");
        var_dump(imageistruecolor($re), imagecolorat($re, 3, 3) === $red);
        $f = fopen('php://memory', 'w+');
        var_dump(imagegif($im, $f), rewind($f), substr(stream_get_contents($f), 0, 6));
        var_dump(imagecreatefromstring(file_get_contents("$dir/a.png")) instanceof GdImage);"#,
    );
    assert_eq!(
        out,
        "int(16711680)\nint(16711680)\nint(0)\nint(10)\nint(8)\n255,0,0,0\nbool(true)\n\
         bool(true)\nbool(true)\nbool(true)\nbool(true)\nstring(6) \"GIF87a\"\nbool(true)\n"
    );
}

#[test]
fn argument_errors() {
    let out = run(
        "errors",
        r#"foreach ([
            fn() => imagecreatetruecolor(0, 1),
            fn() => imagecolorallocate(imagecreate(1, 1), 256, 0, 0),
            fn() => imagefilter(imagecreatetruecolor(1, 1), 99),
            fn() => imagefilter(imagecreatetruecolor(1, 1), IMG_FILTER_BRIGHTNESS),
            fn() => clone imagecreatetruecolor(1, 1),
            fn() => new GdImage(),
            fn() => imagepng(imagecreatetruecolor(1, 1), null, 10),
            fn() => imagepng(imagecreatetruecolor(1, 1), []),
            fn() => imagesetstyle(imagecreatetruecolor(1, 1), []),
            fn() => imagecrop(imagecreatetruecolor(1, 1), ['x' => 0]),
            fn() => imageaffinematrixget(9, 1.0),
            fn() => imagesx(1),
        ] as $f) {
            try { var_dump($f()); } catch (Throwable $e) { echo get_class($e), ': ', $e->getMessage(), "\n"; }
        }"#,
    );
    assert_eq!(
        out,
        "ValueError: imagecreatetruecolor(): Argument #1 ($width) must be greater than 0 and less than 2147483647\n\
         ValueError: imagecolorallocate(): Argument #2 ($red) must be between 0 and 255 (inclusive)\n\
         ValueError: imagefilter(): Argument #2 ($filter) must be one of the IMG_FILTER_* filter constants\n\
         ArgumentCountError: imagefilter() expects exactly 3 arguments, 2 given\n\
         Error: Trying to clone an uncloneable object of class GdImage\n\
         Error: You cannot initialize a GdImage object except through helper functions\n\
         ValueError: imagepng(): Argument #3 ($quality) must be between -1 and 9\n\
         TypeError: imagepng(): Argument #2 ($file) must be a file name or a stream resource, array given\n\
         ValueError: imagesetstyle(): Argument #2 ($style) must not be empty\n\
         ValueError: imagecrop(): Argument #2 ($rectangle) must have a \"y\" key\n\
         ValueError: imageaffinematrixget(): Argument #1 ($type) must be a valid element type\n\
         TypeError: imagesx(): Argument #1 ($image) must be of type GdImage, int given\n"
    );
}

#[test]
fn libgd_messages_become_warnings() {
    let out = run(
        "warnings",
        r#"var_dump(imagecreatefromstring('not an image at all'));
var_dump(imagecreatefromstring("\x89PNG\r\n\x1a\n0000000000"));
var_dump(imagegd(imagecreate(1, 1)));
var_dump(imagecolorat(imagecreate(1, 1), 1, 0));"#,
    );
    assert_eq!(
        out,
        "\nWarning: imagecreatefromstring(): Data is not in a recognized format in test.php on line 2\n\
         bool(false)\n\
         \nWarning: imagecreatefromstring(): gd-png:  fatal libpng error: [30][30][30][30]: invalid chunk type in test.php on line 3\n\
         \nWarning: imagecreatefromstring(): gd-png error: setjmp returns error condition in test.php on line 3\n\
         \nWarning: imagecreatefromstring(): Passed data is not in \"PNG\" format in test.php on line 3\n\
         \nWarning: imagecreatefromstring(): Couldn't create GD Image Stream out of Data in test.php on line 3\n\
         bool(false)\n\
         \nWarning: imagegd(): GD image support has been disabled\n in test.php on line 4\n\
         bool(true)\n\
         \nNotice: imagecolorat(): 1,0 is out of bounds in test.php on line 5\n\
         bool(false)\n"
    );
}

#[test]
fn palette_fonts_matrices_and_info() {
    let out = run(
        "palette",
        r#"$im = imagecreate(4, 4);
        var_dump(imagecolorallocate($im, 1, 2, 3), imagecolorstotal($im), imagecolortransparent($im, 0));
        var_dump(imagecolorset($im, 0, 9, 9, 9), imagecolorsforindex($im, 0)['red'], imagecolorset($im, 5, 0, 0, 0));
        var_dump(imagefontwidth(1), imagefontheight(5), imagefontwidth(99));
        echo implode(',', imageaffinematrixget(IMG_AFFINE_TRANSLATE, ['x' => 2, 'y' => 3])), "\n";
        echo implode(',', imagegetclip($im)), ' ', implode(',', imageresolution($im)), "\n";
        var_dump(imageresolution($im, 300), imageresolution($im)[1]);
        var_dump(gd_info()['GD Version'], imagetypes(), GD_BUNDLED, IMG_FILTER_SCATTER);"#,
    );
    assert_eq!(
        out,
        "int(0)\nint(1)\nint(0)\nNULL\nint(9)\nbool(false)\nint(5)\nint(15)\nint(9)\n\
         1,0,0,1,2,3\n0,0,3,3 96,96\nbool(true)\nint(300)\n\
         string(5) \"2.3.3\"\nint(255)\nint(0)\nint(12)\n"
    );
}

#[test]
fn loaded_fonts_draw() {
    let out = run(
        "font",
        r#"file_put_contents("$dir/f.gdf", pack('i4', 1, 65, 2, 2) . "\x01\x00\x00\x01");
$f = imageloadfont("$dir/f.gdf");
var_dump($f instanceof GdFont, imagefontwidth($f), imagefontheight($f));
$im = imagecreatetruecolor(4, 4);
imagestring($im, $f, 0, 0, 'AA', 0xffffff);
echo imagecolorat($im, 0, 0), ',', imagecolorat($im, 1, 0), ',', imagecolorat($im, 2, 0), "\n";
file_put_contents("$dir/short.gdf", "\x01\x00");
var_dump(imageloadfont("$dir/short.gdf"));"#,
    );
    assert_eq!(
        out,
        "bool(true)\nint(2)\nint(2)\n16777215,0,16777215\n\
         \nWarning: imageloadfont(): End of file while reading header in test.php on line 9\n\
         bool(false)\n"
    );
}
