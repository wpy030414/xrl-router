@echo off
rem Windows/MSVC test runner wrapper (see repo-root .cargo/config.toml).
rem
rem Why: cargo's rustc-link-arg-tests (build.rs injects /MANIFEST:EMBED) only
rem reaches explicit test targets under tests/; the lib unit-test exe (inline
rem #[cfg(test)]) gets no Common-Controls v6 manifest, so the loader binds
rem comctl32 to v5 -> missing v6 entrypoint -> load-time failure 0xC0000139.
rem See docs/DECISIONS.md ADR-052.
rem
rem This script copies src-tauri/tests.manifest next to the test exe as an
rem external manifest (<exe>.manifest). The loader accepts an external manifest
rem when none is embedded. Products that already embed one (the main binary)
rem are unaffected: embedded wins, the external file is ignored.
rem
rem NOTE: comments are ASCII-only on purpose. cmd.exe on Chinese Windows reads
rem .bat files under the GBK codepage; UTF-8 Chinese bytes here get misparsed
rem as commands and pollute output + exit codes.
setlocal EnableExtensions
set "EXE=%~1"
if not defined EXE goto run
if not exist "%EXE%" goto run
if exist "%EXE%.manifest" goto run
copy /y "%~dp0..\tests.manifest" "%EXE%.manifest" >nul 2>&1
:run
%*
exit /b %ERRORLEVEL%
