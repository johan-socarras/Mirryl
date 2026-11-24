# Generates the two images NSIS/Tauri use to dress the installer:
#   installer-header.bmp    150x57   top-left corner of the inner pages
#                                    (License, etc.) - NSIS draws the
#                                    title TO THE RIGHT of this image,
#                                    not on top of it, so they never clash.
#   installer-sidebar.bmp   164x314  full-height panel on Welcome and
#                                    Finish - the text of those pages lives
#                                    in the column next to it, also not on
#                                    top of the image.
#
# NSIS rather than WiX/MSI: WiX only offers a full-page background with its
# own text drawn ON TOP at a fixed position, and anything placed there clashes
# with that text sooner or later. NSIS keeps image and text in separate
# columns.
#
# Colours from Mirryl's palette: the header uses the light theme (bg #f3f5ff,
# accent #07717f) to match the white background of the inner pages; the side
# panel uses the DARK theme (bg #070914, accent #65e7ff) because the Mirryl
# icon already carries its own dark background - a dark panel lets it blend in
# without a visible box around it.
#
# NOTE: run with powershell.exe (5.1), NOT pwsh: System.Drawing can be broken
# in pwsh 7.

Add-Type -AssemblyName System.Drawing

$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$iconSrc = Join-Path $here "icons\128x128@2x.png"
$headerOut = Join-Path $here "installer-header.bmp"
$sidebarOut = Join-Path $here "installer-sidebar.bmp"

$colBgLight    = [System.Drawing.ColorTranslator]::FromHtml("#f3f5ff")
$colSurface    = [System.Drawing.ColorTranslator]::FromHtml("#ffffff")
$colAccentLight = [System.Drawing.ColorTranslator]::FromHtml("#07717f")

$colBgDark     = [System.Drawing.ColorTranslator]::FromHtml("#070914")
$colSurfaceDark = [System.Drawing.ColorTranslator]::FromHtml("#111525")
$colAccentDark = [System.Drawing.ColorTranslator]::FromHtml("#65e7ff")
$colTextDark   = [System.Drawing.ColorTranslator]::FromHtml("#f7f8ff")
$colMutedDark  = [System.Drawing.ColorTranslator]::FromHtml("#a8aec7")

function New-HighQualityGraphics($bmp) {
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
    $g.TextRenderingHint = [System.Drawing.Text.TextRenderingHint]::AntiAliasGridFit
    return $g
}

# ---------------- header (150x57) ----------------
$hw = 150; $hh = 57
$header = New-Object System.Drawing.Bitmap $hw, $hh, ([System.Drawing.Imaging.PixelFormat]::Format24bppRgb)
$g = New-HighQualityGraphics $header
$g.Clear($colSurface)

$icon = [System.Drawing.Image]::FromFile($iconSrc)
$iconSize = 36
$iconY = [int](($hh - $iconSize) / 2)
$g.DrawImage($icon, 10, $iconY, $iconSize, $iconSize)
$icon.Dispose()

$fontHeader = New-Object System.Drawing.Font("Segoe UI Semibold", 13, [System.Drawing.FontStyle]::Regular, [System.Drawing.GraphicsUnit]::Pixel)
$brushHeader = New-Object System.Drawing.SolidBrush($colAccentLight)
$g.DrawString("Mirryl", $fontHeader, $brushHeader, 10 + $iconSize + 8, [int](($hh - 17) / 2))

$g.Dispose()
$header.Save($headerOut, [System.Drawing.Imaging.ImageFormat]::Bmp)
$header.Dispose()
Write-Output "header -> $headerOut"

# ---------------- sidebar (164x314) ----------------
$sw = 164; $sh = 314
$sidebar = New-Object System.Drawing.Bitmap $sw, $sh, ([System.Drawing.Imaging.PixelFormat]::Format24bppRgb)
$g2 = New-HighQualityGraphics $sidebar

$rect = New-Object System.Drawing.Rectangle 0, 0, $sw, $sh
$gradBrush = New-Object System.Drawing.Drawing2D.LinearGradientBrush($rect, $colBgDark, $colSurfaceDark, 90)
$g2.FillRectangle($gradBrush, $rect)
$gradBrush.Dispose()

$icon2 = [System.Drawing.Image]::FromFile($iconSrc)
$bigIconSize = 84
$iconX2 = [int](($sw - $bigIconSize) / 2)
$g2.DrawImage($icon2, $iconX2, 64, $bigIconSize, $bigIconSize)
$icon2.Dispose()

$fmtCenter = New-Object System.Drawing.StringFormat
$fmtCenter.Alignment = [System.Drawing.StringAlignment]::Center

$fontTitle = New-Object System.Drawing.Font("Segoe UI Semibold", 19, [System.Drawing.FontStyle]::Regular, [System.Drawing.GraphicsUnit]::Pixel)
$brushTitle = New-Object System.Drawing.SolidBrush($colTextDark)
$g2.DrawString("Mirryl", $fontTitle, $brushTitle, [System.Drawing.RectangleF]::new(0, 168, $sw, 28), $fmtCenter)

$fontTag = New-Object System.Drawing.Font("Segoe UI", 10, [System.Drawing.FontStyle]::Regular, [System.Drawing.GraphicsUnit]::Pixel)
$brushTag = New-Object System.Drawing.SolidBrush($colMutedDark)
# narrow (164px): the sentence is broken into two lines by hand instead of
# letting the text engine split it somewhere ugly
$g2.DrawString("Mirror and control", $fontTag, $brushTag, [System.Drawing.RectangleF]::new(0, 204, $sw, 18), $fmtCenter)
$g2.DrawString("your Android device", $fontTag, $brushTag, [System.Drawing.RectangleF]::new(0, 222, $sw, 18), $fmtCenter)

# narrow accent strip at the very bottom
$penAccent = New-Object System.Drawing.Pen($colAccentDark, 3)
$g2.DrawLine($penAccent, 0, $sh - 2, $sw, $sh - 2)

$g2.Dispose()
$sidebar.Save($sidebarOut, [System.Drawing.Imaging.ImageFormat]::Bmp)
$sidebar.Dispose()
Write-Output "sidebar -> $sidebarOut"
