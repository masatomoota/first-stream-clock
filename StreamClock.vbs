' StreamClock launcher (no console flash)
' ダブルクリックで起動。コンソールウィンドウを一切表示せずに時計だけ起動します。
Dim sh, dir
Set sh = CreateObject("WScript.Shell")
dir = Left(WScript.ScriptFullName, InStrRev(WScript.ScriptFullName, "\"))
sh.Run "powershell -NoProfile -ExecutionPolicy Bypass -STA -WindowStyle Hidden -File """ & dir & "StreamClock.ps1""", 0, False
