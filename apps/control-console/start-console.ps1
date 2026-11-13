$here = Split-Path -Parent $MyInvocation.MyCommand.Path
Set-Location $here
Start-Process "http://localhost:5173"
python serve.py
