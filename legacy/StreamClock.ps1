# StreamClock - JST digital clock overlay (black bg / green text)
# Windows PowerShell 5.1 (WPF) で動作。ビルド不要。
# 操作:
#   ドラッグ          : 日付/時刻エリアをドラッグでウィンドウ移動
#   マウスホイール    : 背景(黒)の不透明度を調整
#   ↑ / ↓ キー       : 背景(黒)の不透明度を調整
#   ストップウォッチを
#   ダブルクリック    : ストップ → リセット → スタート の順に切替
#   右クリック        : メニュー(最前面切替/不透明度プリセット/終了)
#   Esc               : 終了

Add-Type -AssemblyName PresentationFramework, PresentationCore, WindowsBase, System.Xaml

$xaml = @'
<Window xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
        xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
        Title="StreamClock"
        Width="480" Height="320"
        MinWidth="160" MinHeight="110"
        WindowStyle="None"
        AllowsTransparency="True"
        ResizeMode="CanResizeWithGrip"
        Topmost="True"
        UseLayoutRounding="True"
        WindowStartupLocation="CenterScreen"
        Background="#FF000000">
  <Grid x:Name="Root">
    <Grid.ContextMenu>
      <ContextMenu>
        <MenuItem x:Name="MiTop" Header="最前面に表示" IsCheckable="True" IsChecked="True"/>
        <Separator/>
        <MenuItem x:Name="MiOp100" Header="背景の不透明度 100%"/>
        <MenuItem x:Name="MiOp75"  Header="背景の不透明度 75%"/>
        <MenuItem x:Name="MiOp50"  Header="背景の不透明度 50%"/>
        <MenuItem x:Name="MiOp25"  Header="背景の不透明度 25%"/>
        <MenuItem x:Name="MiOp0"   Header="背景の不透明度 0% (完全透過)"/>
        <Separator/>
        <MenuItem x:Name="MiExit"  Header="終了"/>
      </ContextMenu>
    </Grid.ContextMenu>

    <Viewbox Stretch="Uniform" Margin="22">
      <StackPanel HorizontalAlignment="Center">
        <TextBlock x:Name="DateText" Text="2026/06/12"
                   FontFamily="Consolas" FontSize="26"
                   Foreground="#00FF66" HorizontalAlignment="Center"
                   Margin="0,0,0,2">
          <TextBlock.Effect>
            <DropShadowEffect Color="#00FF66" BlurRadius="6" ShadowDepth="0"/>
          </TextBlock.Effect>
        </TextBlock>

        <TextBlock x:Name="TimeText" Text="00:00:00"
                   FontFamily="Consolas" FontWeight="Bold" FontSize="96"
                   Foreground="#00FF66" HorizontalAlignment="Center">
          <TextBlock.Effect>
            <DropShadowEffect Color="#00FF66" BlurRadius="14" ShadowDepth="0"/>
          </TextBlock.Effect>
        </TextBlock>

        <TextBlock x:Name="SwText" Text="00:00:00"
                   FontFamily="Consolas" FontWeight="Bold" FontSize="74"
                   Foreground="#2E7D32" HorizontalAlignment="Center"
                   Background="Transparent" Padding="6,2"
                   ToolTip="ダブルクリックで ストップ→リセット→スタート">
          <TextBlock.Effect>
            <DropShadowEffect Color="#00FF66" BlurRadius="12" ShadowDepth="0"/>
          </TextBlock.Effect>
        </TextBlock>
      </StackPanel>
    </Viewbox>
  </Grid>
</Window>
'@

$reader = New-Object System.Xml.XmlNodeReader ([xml]$xaml)
$script:window = [Windows.Markup.XamlReader]::Load($reader)

# --- element refs ---
$script:DateText = $script:window.FindName('DateText')
$script:TimeText = $script:window.FindName('TimeText')
$script:SwText   = $script:window.FindName('SwText')

# --- JST timezone ---
$script:jst = [System.TimeZoneInfo]::FindSystemTimeZoneById('Tokyo Standard Time')

# --- stopwatch ---
$script:sw = New-Object System.Diagnostics.Stopwatch

# --- stopwatch state colors ---
$script:brZero = New-Object System.Windows.Media.SolidColorBrush ([System.Windows.Media.Color]::FromRgb(0x2E,0x7D,0x32)) # 待機: 暗い緑
$script:brRun  = New-Object System.Windows.Media.SolidColorBrush ([System.Windows.Media.Color]::FromRgb(0x00,0xFF,0x66)) # 計測中: 明るい緑
$script:brStop = New-Object System.Windows.Media.SolidColorBrush ([System.Windows.Media.Color]::FromRgb(0xFF,0xB3,0x00)) # 停止: 橙

function Update-SwColor {
  if ($script:sw.IsRunning) {
    $script:SwText.Foreground = $script:brRun
  } elseif ($script:sw.Elapsed.Ticks -gt 0) {
    $script:SwText.Foreground = $script:brStop
  } else {
    $script:SwText.Foreground = $script:brZero
  }
}

# --- background opacity ---
$script:bgAlpha = 255
function Set-BgAlpha([int]$a) {
  $script:bgAlpha = [Math]::Max(0, [Math]::Min(255, $a))
  $c = [System.Windows.Media.Color]::FromArgb([byte]$script:bgAlpha, [byte]0, [byte]0, [byte]0)
  $script:window.Background = New-Object System.Windows.Media.SolidColorBrush $c
}

# --- clock tick ---
$script:timer = New-Object System.Windows.Threading.DispatcherTimer
$script:timer.Interval = [TimeSpan]::FromMilliseconds(100)
$script:timer.Add_Tick({
  $now = [System.TimeZoneInfo]::ConvertTimeFromUtc([DateTime]::UtcNow, $script:jst)
  $script:DateText.Text = $now.ToString('yyyy/MM/dd')
  $script:TimeText.Text = $now.ToString('HH:mm:ss')
  $ts = $script:sw.Elapsed
  $h  = [int][Math]::Floor($ts.TotalHours)
  $script:SwText.Text = '{0:00}:{1:00}:{2:00}' -f $h, $ts.Minutes, $ts.Seconds
})

# --- drag to move (date/time area) ---
$script:window.Add_MouseLeftButtonDown({
  try { $script:window.DragMove() } catch {}
})

# --- stopwatch: double-click cycles Stop -> Reset -> Start ---
$script:SwText.Add_MouseLeftButtonDown({
  param($s, $e)
  if ($e.ClickCount -eq 2) {
    if ($script:sw.IsRunning) {
      $script:sw.Stop()              # ストップ
    } elseif ($script:sw.Elapsed.Ticks -gt 0) {
      $script:sw.Reset()             # リセット
    } else {
      $script:sw.Start()             # スタート
    }
    Update-SwColor
  }
  $e.Handled = $true                 # ストップウォッチ上ではウィンドウ移動させない
})

# --- mouse wheel: adjust background opacity ---
$script:window.Add_MouseWheel({
  param($s, $e)
  $step = if ($e.Delta -gt 0) { 13 } else { -13 }
  Set-BgAlpha ($script:bgAlpha + $step)
})

# --- keyboard ---
$script:window.Add_KeyDown({
  param($s, $e)
  switch ($e.Key) {
    'Escape' { $script:window.Close() }
    'Up'     { Set-BgAlpha ($script:bgAlpha + 13) }
    'Down'   { Set-BgAlpha ($script:bgAlpha - 13) }
  }
})

# --- context menu ---
$script:window.FindName('MiExit').Add_Click({ $script:window.Close() })
$script:window.FindName('MiOp100').Add_Click({ Set-BgAlpha 255 })
$script:window.FindName('MiOp75').Add_Click({ Set-BgAlpha 191 })
$script:window.FindName('MiOp50').Add_Click({ Set-BgAlpha 128 })
$script:window.FindName('MiOp25').Add_Click({ Set-BgAlpha 64 })
$script:window.FindName('MiOp0').Add_Click({ Set-BgAlpha 0 })
$miTop = $script:window.FindName('MiTop')
$miTop.Add_Click({ $script:window.Topmost = $miTop.IsChecked }.GetNewClosure())

# --- start ---
Set-BgAlpha 255
Update-SwColor
$script:window.Add_Closed({ $script:timer.Stop() })
$script:timer.Start()
[void]$script:window.ShowDialog()
