# wisp

**用 Rust 從零寫成的精簡 WebAssembly 執行環境：二進位解碼器、驗證器、暫存器式直譯器、
單趟產生 AArch64 機器碼的基線編譯器，以及具備能力式（capability）沙箱的 WASI preview 1。**

[English](README.md) · [設計文件](docs/DESIGN.md) · [規格測試結果](docs/spec-results.md) · [效能測試](docs/benchmarks.md) · [導讀](docs/導讀.zh-TW.md)

wisp 用大約 12,600 行、幾乎不依賴外部套件的 Rust，把 WebAssembly 引擎需要的每一層都做出來，
而且每項結果都能重跑驗證：

- **符合規格。** 官方規格測試（WebAssembly/spec 標籤 `wg-2.0`）實際執行的 27,416 個斷言，
  直譯器與編譯器**兩者**全部通過；每個被拒絕的模組，錯誤訊息也和官方參考直譯器一致。
  測試執行器是 wisp 自己寫的，`wast` 套件只用來解析腳本的文字格式。
- **真實程式。** 用 `rustc --target wasm32-wasip1` 編譯的 Rust 程式（用到 `serde_json`、
  `miniz_oxide`、`regex` 與標準函式庫的檔案系統 API），輸出和原生版本完全相同。
- **真正的編譯器。** 基線編譯器有自己的 AArch64 指令編碼器（逐字和 clang 組譯結果比對），
  在 `MAP_JIT` 記憶體中執行，能編譯 2.0 除了 SIMD 以外的所有指令；端到端比 wasmtime/Cranelift
  慢 1.3–3.5 倍，大約和 wasmtime 的 Winch 基線編譯器同級，但編譯速度比兩者都快。
- **守得住的沙箱。** 客程式的檔案存取，是從預先開放的目錄描述子一段一段解析路徑；
  測試證明 `..`、絕對路徑符號連結、含 `..` 的符號連結與連結迴圈都無法跑到外面。

## 示範

```text
$ wisp run raytrace.wasm 320
320x240: fnv 9427c78dcb5b8249, mean 156.337630
$ wisp compile regex.wasm
1355660 bytes, decode 380.167µs, validate 5.063583ms, compile 8.297583ms (1615 funcs, 2226852 bytes of code), translate 917ns (0 funcs interpreted)
regex.wasm: 13.7 ms total, 98.6 MB/s
$ wisp run --dir box::/sandbox sandbox.wasm /sandbox
inside.txt: READ "hi"
../outside/secret.txt: denied (Uncategorized)
escape/secret.txt (dir symlink): denied (Uncategorized)
abs/hosts (absolute symlink): denied (Uncategorized)
loop (symlink loop): denied (FilesystemLoop)
```

（`Uncategorized` 是 Rust 標準函式庫對 WASI `ENOTCAPABLE` 的顯示方式。）

嵌入用法：

```rust
use wisp::{Engine, Instance, Module, Store, Val};

let engine = Engine::default(); // Apple Silicon 上用編譯器，其他平台用直譯器
let module = Module::new(&engine, &wasm_bytes)?;
let mut store = Store::new(&engine, ());
let instance = Instance::new(&mut store, &module, &[])?;
let add = instance.get_func(&store, "add").unwrap();
assert_eq!(add.call(&mut store, &[Val::I32(2), Val::I32(3)])?, vec![Val::I32(5)]);
```

`Linker` 依名稱解析匯入並定義主機函式（`Caller` 讓主機函式拿到 store 資料與呼叫端的記憶體）；
`wisp::wasi::add_to_linker` 加入 WASI。陷阱（trap）以 `Err(Error::Trap(..))` 回傳，store 之後仍可繼續使用。

## 運作方式

```text
 .wasm 位元組
     │  binary::module  — 各區段、照規格溢位規則解 LEB128
     ▼
 ModuleData ──► validate — 規格附錄的驗證演算法；記下每個函式的最大堆疊高度
     │
     ├──► interp::translate ──► 以槽位定址的位元組碼 ──► interp::exec（迴圈 + match）
     │
     └──► jit::compile ──► jit::a64 編碼器 ──► MAP_JIT 記憶體 ──► 進入跳板
     runtime：Store / Instance / VmCtx（#[repr(C)]，兩種引擎和主機共用）
     wasi：建立在能力式路徑解析之上（openat/readlinkat，絕不讓核心替我們跟隨連結）
```

**直譯器。** Wasm 運算元堆疊的高度在編譯時就確定，所以深度 *d* 的運算元永遠放在框架的
`nlocals + d` 槽位。轉譯器把堆疊碼轉成以槽位為運算元的三位址指令（每道 16 位元組），
運算元可以直接指向區域變數（`local.get` 不產生指令）、有立即值形式、比較與分支合併、常數摺疊，
`local.set` 會直接改寫產生該值的指令。呼叫時被呼叫者的框架疊在呼叫者的引數槽位上，
引數不用複製，結果也剛好落在呼叫者預期的位置；wasm 之間的呼叫不會在原生堆疊上遞迴。

**編譯器。** 每個函式只走一趟，維護一個抽象運算元堆疊，每個項目可能在暫存器、溢出槽位、
條件旗標（尚未具體化的比較結果，所以 `i32.lt_s; br_if` 會變成 `cmp; b.lt`）或是常數。
20 個通用與 30 個向量暫存器當快取；區塊邊界時一切都放回槽位，所以合流點不需要調和狀態。
`x28` 放實例上下文，`x27`/`x26` 放記憶體基底與大小，`x25` 放執行環境。記憶體存取以 `x26`
明確檢查邊界（`add; cmp; b.hi`）。陷阱會跳到一段結束程式，把 `sp` 還原成進入跳板保存的值並回傳
陷阱代碼，由 Rust 轉成錯誤。模組內的呼叫是直接 `bl`；匯入與 `call_indirect` 透過函式參照，
也能通往主機函式與直譯器。細節與捨棄的方案見 [docs/DESIGN.md](docs/DESIGN.md)。

## 結果

### 規格測試

`cargo test --release --test spec` 會下載 WebAssembly/spec 的 `wg-2.0` 標籤（commit `fffc6e12`），
用兩種引擎各跑一次全部 90 個 `test/core/*.wast`：

| 引擎 | 通過 | 失敗 | 略過 |
|---|---:|---:|---:|
| 直譯器 | 27,416 | 0 | 581 |
| 編譯器 | 27,416 | 0 | 581 |

略過的 581 個是 `module quote` 斷言，它們測的是文字格式解析器（wisp 沒有）。`test/core/simd/`
沒有執行：SIMD 尚未實作。各檔案數字見 [docs/spec-results.md](docs/spec-results.md)。

### 真實程式與沙箱

`cargo test --release --test programs` 會把 `tests/programs` 同時編成 `wasm32-wasip1` 與原生版本，
在兩種引擎下執行並要求輸出相同：質數篩、遞迴費氏數列、f64 矩陣乘法、光線追蹤、DEFLATE 往返
（`miniz_oxide`）、JSON（`serde_json`）、正規表示式搜尋（`regex`，1.3 MB 的模組）、在預開目錄中做
檔案 I/O 的程式，以及十二個沙箱逃脫嘗試（九個逃脫全部被擋，外面沒有產生任何檔案）。

### 差異模糊測試

`tools/fuzz-diff` 用 `wasm-smith` 產生模組（2.0 功能、無 SIMD、NaN 正規化、迴圈有上限），
在兩種引擎下呼叫每個匯出函式，比對結果、陷阱、記憶體與全域變數。最近一次（`--cases 5000 --seed 1`）：
5,000 個模組、9,190 次呼叫、0 個不一致、0 個模組被 wisp 拒絕。產生的模組很小（平均約兩次呼叫），
所以它是規格測試的補充，而不是替代。

### 效能

`python3 bench/run.py`（方法：整個行程依序跑 5 次取中位數、牆鐘時間、輸出和原生版本比對；
Apple M5、macOS 27；wasmtime 49.0.1、wasmer 7.5.0 官方發行版）。單位秒，越小越好，含啟動與編譯。

| 程式 | 原生 | wisp 編譯器 | wisp 直譯器 | wasmtime Cranelift | wasmtime Winch | wasmer Cranelift | wasmer Singlepass |
|---|---:|---:|---:|---:|---:|---:|---:|
| primes 20M | 0.035 | 0.119 | 0.339 | 0.048 | 0.070 | 0.063 | 0.076 |
| fib 34 | 0.011 | 0.022 | 0.110 | 0.025 | 0.030 | 0.037 | 0.027 |
| matmul 300 | 0.005 | 0.054 | 0.300 | 0.027 | 0.047 | 0.040 | 0.529 |
| raytrace 400 | 0.019 | 0.051 | 0.263 | 0.030 | 0.064 | 0.043 | 1.076 |
| compress 8 MB | 0.198 | 0.706 | 3.531 | 0.307 | 0.522 | 0.319 | 0.507 |
| json 60k | 0.113 | 0.261 | 1.747 | 0.139 | 0.322 | 0.150 | 0.270 |
| regex 4 MB | 0.081 | 0.309 | 2.480 | 0.203 | 0.338 | 0.208 | 0.332 |

wisp 比較慢的地方，直說：除了 `fib` 之外，全面輸給 wasmtime/Cranelift（1.3–3.5 倍），
在 `primes`、`compress`、`matmul` 也輸給 Winch。主要成本是每次記憶體存取都要明確檢查邊界
（wasmtime 用保護頁）、區域變數一律放在記憶體、以及每個區塊邊界都要把所有值寫回。
直譯器比編譯器慢 5–15 倍。

`regex.wasm`（1.36 MB）只編譯的牆鐘時間：wisp 16 ms（84 MB/s，單執行緒）；wasmtime Cranelift 72 ms、
Winch 24 ms；wasmer Cranelift 80 ms、Singlepass 31 ms（其他執行環境會平行編譯，並把產物寫到磁碟）。

## 範圍與限制

- **已實作：** WebAssembly 2.0 核心（不含 SIMD）：多值回傳、bulk memory、參照型別、多個表格、
  符號延伸、飽和轉換、可變全域變數的匯入匯出。兩種引擎都完整支援；編譯器對 `memory.grow/fill/copy/init`、
  `data.drop` 與表格批次操作會呼叫 Rust 輔助函式。
- **未實作：** SIMD（`v128` 會回報「unsupported」）、執行緒，以及所有 3.0 提案（GC、例外、尾呼叫、
  memory64、多記憶體、擴充常數）。
- **編譯器平台：** 只支援 AArch64 macOS。Linux/AArch64 的程式路徑有寫但未測試，因此停用；其他平台使用直譯器。
- **編譯器品質：** 沒有跨區塊暫存器配置、區域變數不放暫存器、用明確邊界檢查而非保護頁；
  燃料（fuel）以直線區塊為單位扣除，所以燃料陷阱最多可能早一個區塊觸發。
- **WASI：** 不支援 socket（`sock_*` 回傳 `ENOTSUP`）；`poll_oneoff` 支援時鐘訂閱，檔案描述子訂閱一律立即回報就緒；
  權限（rights）只記錄，實際只強制讀寫模式；`fd_readdir` 每次呼叫都重新讀取目錄。
- **嵌入：** `Store` 只能單執行緒使用；還沒有 C API 標頭檔。
- 效能數字來自單一機器與一組輸入，而且包含啟動時間。

## 相關專案

wisp 不宣稱任何新穎性，它只是一個從零寫成、帶 AArch64 基線編譯器、結果公開透明的精簡執行環境。最接近的專案：

- **wasmtime** 的 **Cranelift**（最佳化編譯器）與 **Winch**（單趟基線編譯器）。Winch 和 wisp 的編譯器最像；
  wasmtime 完整得多也快得多。
- **wasmer**，有 Cranelift、LLVM 與 **Singlepass** 後端。
- **V8 Liftoff** 與 **SpiderMonkey 的 Wasm 基線編譯器**：在值堆疊上加暫存器快取的單趟編譯器；
  wisp 的編譯器用的是同一個想法的簡化版。
- **wasmi**：Rust 寫的直譯器，暫存器式位元組碼和 wisp 的槽位定址直譯器屬於同一類設計。
- **wasm3**（threaded code 直譯器）、**WAMR**（直譯器加 AOT/JIT）、**wazero**（Go，直譯器加編譯器）。

## 建置與測試

需要 Rust 1.88 以上（開發時用 1.98）；程式測試需要 `rustup target add wasm32-wasip1`。

```sh
cargo build --release            # target/release/wisp
make test                        # 單元、API、規格測試（兩種引擎）、客程式
make spec-report                 # 重新產生 docs/spec-results.md
make fuzz                        # 差異模糊測試，2,000 個模組
make bench                       # 需要 PATH 上有 wasmtime/wasmer，或設定 $WASMTIME/$WASMER
make lint                        # rustfmt + clippy -D warnings
```

規格測試第一次使用時會用 `curl` 下載到 `target/spec`（或設定 `WISP_SPEC_DIR`）。MIT 授權。
