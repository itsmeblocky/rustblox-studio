--!nocheck
-- Rustblox Studio — Roblox Luau prelude.
--
-- Datatypes and enums that Roblox injects before a place's scripts run. The
-- runtime (Rust) installs `Instance`, `game`, `workspace`, `task`, `typeof`,
-- `require`, `print`/`warn` and the services on top of these definitions.
--
-- Every value carries a `__datatype` tag so the Rust side can convert to/from
-- stored instance properties without guessing (see convert.rs).

local sqrt = math.sqrt
local cos, sin, rad, abs = math.cos, math.sin, math.rad, math.abs
local floor = math.floor

--------------------------------------------------------------------------------
-- Vector2
--------------------------------------------------------------------------------

local Vector2 = {}
local v2methods = {}
local v2meta = {}

local function isv2(v)
	return type(v) == "table" and v.__datatype == "Vector2"
end

local function v2(x, y)
	return setmetatable({ __datatype = "Vector2", _x = x, _y = y }, v2meta)
end

function Vector2.new(x, y)
	if isv2(x) then
		return v2(x._x, x._y)
	end
	return v2(x or 0, y or 0)
end

v2meta.__index = function(self, k)
	if k == "X" or k == "x" then return self._x end
	if k == "Y" or k == "y" then return self._y end
	if k == "Magnitude" then return sqrt(self._x * self._x + self._y * self._y) end
	if k == "Unit" then
		local m = sqrt(self._x * self._x + self._y * self._y)
		if m == 0 then return v2(0, 0) end
		return v2(self._x / m, self._y / m)
	end
	return v2methods[k]
end
v2meta.__add = function(a, b) return v2(a._x + b._x, a._y + b._y) end
v2meta.__sub = function(a, b) return v2(a._x - b._x, a._y - b._y) end
v2meta.__mul = function(a, b)
	if isv2(a) then return v2(a._x * b, a._y * b) end
	return v2(a * b._x, a * b._y)
end
v2meta.__div = function(a, b)
	if isv2(a) then return v2(a._x / b, a._y / b) end
	return v2(a / b._x, a / b._y)
end
v2meta.__unm = function(a) return v2(-a._x, -a._y) end
v2meta.__eq = function(a, b) return a._x == b._x and a._y == b._y end
v2meta.__tostring = function(a) return string.format("%g, %g", a._x, a._y) end
Vector2.__index = Vector2
v2methods.Lerp = function(a, b, t) return v2(a._x + (b._x - a._x) * t, a._y + (b._y - a._y) * t) end
v2methods.Dot = function(a, b) return a._x * b._x + a._y * b._y end

--------------------------------------------------------------------------------
-- Vector3
--------------------------------------------------------------------------------

local Vector3 = {}
local v3methods = {}
local v3meta = {}

local function isv3(v)
	return type(v) == "table" and v.__datatype == "Vector3"
end

local function v3(x, y, z)
	return setmetatable({ __datatype = "Vector3", _x = x, _y = y, _z = z }, v3meta)
end

function Vector3.new(x, y, z)
	if isv3(x) then
		return v3(x._x, x._y, x._z)
	end
	if type(x) == "number" and y == nil and z == nil then
		return v3(x, 0, 0)
	end
	return v3(x or 0, y or 0, z or 0)
end

function Vector3.FromNormalId(normal)
	local name = typeof(normal) == "EnumItem" and normal.Name or tostring(normal)
	if name == "Right" then return v3(1, 0, 0) end
	if name == "Left" then return v3(-1, 0, 0) end
	if name == "Top" then return v3(0, 1, 0) end
	if name == "Bottom" then return v3(0, -1, 0) end
	if name == "Back" then return v3(0, 0, 1) end
	return v3(0, 0, -1)
end

function Vector3.FromAxis(axis)
	local name = typeof(axis) == "EnumItem" and axis.Name or tostring(axis)
	if name == "Y" then return v3(0, 1, 0) end
	if name == "Z" then return v3(0, 0, 1) end
	return v3(1, 0, 0)
end

v3meta.__index = function(self, k)
	if k == "X" or k == "x" then return self._x end
	if k == "Y" or k == "y" then return self._y end
	if k == "Z" or k == "z" then return self._z end
	if k == "Magnitude" then
		return sqrt(self._x * self._x + self._y * self._y + self._z * self._z)
	end
	if k == "Unit" then
		local m = sqrt(self._x * self._x + self._y * self._y + self._z * self._z)
		if m == 0 then return v3(0, 0, 0) end
		return v3(self._x / m, self._y / m, self._z / m)
	end
	return v3methods[k]
end
v3meta.__add = function(a, b) return v3(a._x + b._x, a._y + b._y, a._z + b._z) end
v3meta.__sub = function(a, b) return v3(a._x - b._x, a._y - b._y, a._z - b._z) end
v3meta.__mul = function(a, b)
	if isv3(a) then return v3(a._x * b, a._y * b, a._z * b) end
	return v3(a * b._x, a * b._y, a * b._z)
end
v3meta.__div = function(a, b)
	if isv3(a) then return v3(a._x / b, a._y / b, a._z / b) end
	return v3(a / b._x, a / b._y, a / b._z)
end
v3meta.__unm = function(a) return v3(-a._x, -a._y, -a._z) end
v3meta.__eq = function(a, b) return a._x == b._x and a._y == b._y and a._z == b._z end
v3meta.__tostring = function(a) return string.format("%g, %g, %g", a._x, a._y, a._z) end
Vector3.__index = Vector3
v3methods.Lerp = function(a, b, t)
	return v3(a._x + (b._x - a._x) * t, a._y + (b._y - a._y) * t, a._z + (b._z - a._z) * t)
end
v3methods.Dot = function(a, b) return a._x * b._x + a._y * b._y + a._z * b._z end
v3methods.Cross = function(a, b)
	return v3(
		a._y * b._z - a._z * b._y,
		a._z * b._x - a._x * b._z,
		a._x * b._y - a._y * b._x
	)
end

--------------------------------------------------------------------------------
-- Color3
--------------------------------------------------------------------------------

local Color3 = {}
local c3methods = {}
local c3meta = {}

local function isc3(v)
	return type(v) == "table" and v.__datatype == "Color3"
end

local function c3(r, g, b)
	return setmetatable({ __datatype = "Color3", _r = r, _g = g, _b = b }, c3meta)
end

function Color3.new(r, g, b)
	if isc3(r) then return c3(r._r, r._g, r._b) end
	return c3(r or 0, g or 0, b or 0)
end

function Color3.fromRGB(r, g, b)
	return c3((r or 0) / 255, (g or 0) / 255, (b or 0) / 255)
end

function Color3.fromHSV(h, s, v)
	h = h % 1
	local i = floor(h * 6)
	local f = h * 6 - i
	local p = v * (1 - s)
	local q = v * (1 - f * s)
	local t = v * (1 - (1 - f) * s)
	local r, g, b
	if i == 0 then r, g, b = v, t, p
	elseif i == 1 then r, g, b = q, v, p
	elseif i == 2 then r, g, b = p, v, t
	elseif i == 3 then r, g, b = p, q, v
	elseif i == 4 then r, g, b = t, p, v
	else r, g, b = v, p, q end
	return c3(r, g, b)
end

function Color3.toHSV(color)
	local r, g, b = color._r, color._g, color._b
	local maxc, minc = math.max(r, g, b), math.min(r, g, b)
	local d = maxc - minc
	local h = 0
	if d ~= 0 then
		if maxc == r then h = ((g - b) / d) % 6
		elseif maxc == g then h = (b - r) / d + 2
		else h = (r - g) / d + 4 end
		h = h / 6
	end
	return h, (maxc == 0 and 0 or d / maxc), maxc
end

v3meta.__tostring = function(a) return string.format("%g, %g, %g", a._x, a._y, a._z) end
c3meta.__index = function(self, k)
	if k == "R" or k == "r" then return self._r end
	if k == "G" or k == "g" then return self._g end
	if k == "B" or k == "b" then return self._b end
	return c3methods[k]
end
c3meta.__add = function(a, b) return c3(a._r + b._r, a._g + b._g, a._b + b._b) end
c3meta.__sub = function(a, b) return c3(a._r - b._r, a._g - b._g, a._b - b._b) end
c3meta.__mul = function(a, b)
	if isc3(a) then return c3(a._r * b, a._g * b, a._b * b) end
	return c3(a * b._r, a * b._g, a * b._b)
end
c3meta.__eq = function(a, b) return a._r == b._r and a._g == b._g and a._b == b._b end
c3meta.__tostring = function(a) return string.format("%g, %g, %g", a._r, a._g, a._b) end
Color3.__index = Color3
c3methods.Lerp = function(a, b, t)
	return c3(a._r + (b._r - a._r) * t, a._g + (b._g - a._g) * t, a._b + (b._b - a._b) * t)
end

--------------------------------------------------------------------------------
-- CFrame
--------------------------------------------------------------------------------

local CFrame = {}
local cfmethods = {}
local cfmeta = {}

local function iscf(v)
	return type(v) == "table" and v.__datatype == "CFrame"
end

local function makecf(x, y, z, m11, m12, m13, m21, m22, m23, m31, m32, m33)
	return setmetatable({
		__datatype = "CFrame",
		_x = x, _y = y, _z = z,
		_m11 = m11, _m12 = m12, _m13 = m13,
		_m21 = m21, _m22 = m22, _m23 = m23,
		_m31 = m31, _m32 = m32, _m33 = m33,
	}, cfmeta)
end

local IDENTITY = { 1, 0, 0, 0, 1, 0, 0, 0, 1 }

local function mul3(a, b)
	return {
		a[1] * b[1] + a[2] * b[4] + a[3] * b[7],
		a[1] * b[2] + a[2] * b[5] + a[3] * b[8],
		a[1] * b[3] + a[2] * b[6] + a[3] * b[9],
		a[4] * b[1] + a[5] * b[4] + a[6] * b[7],
		a[4] * b[2] + a[5] * b[5] + a[6] * b[8],
		a[4] * b[3] + a[5] * b[6] + a[6] * b[9],
		a[7] * b[1] + a[8] * b[4] + a[9] * b[7],
		a[7] * b[2] + a[8] * b[5] + a[9] * b[8],
		a[7] * b[3] + a[8] * b[6] + a[9] * b[9],
	}
end

local function mat(cf)
	return { cf._m11, cf._m12, cf._m13, cf._m21, cf._m22, cf._m23, cf._m31, cf._m32, cf._m33 }
end

local function frompos(x, y, z, m)
	m = m or IDENTITY
	return makecf(x, y, z, m[1], m[2], m[3], m[4], m[5], m[6], m[7], m[8], m[9])
end

function CFrame.new(...)
	local n = select("#", ...)
	local a, b, c, d, e, f, g, h, i, j, k, l = ...
	if n == 0 then
		return frompos(0, 0, 0)
	elseif n == 1 then
		if iscf(a) then
			return makecf(a._x, a._y, a._z, a._m11, a._m12, a._m13, a._m21, a._m22, a._m23, a._m31, a._m32, a._m33)
		end
		return frompos(a._x, a._y, a._z)
	elseif n == 2 then
		-- position, lookAt
		local px, py, pz = a._x, a._y, a._z
		local lx, ly, lz = b._x, b._y, b._z
		return CFrame.lookAt(Vector3.new(px, py, pz), Vector3.new(lx, ly, lz))
	elseif n == 3 then
		return frompos(a, b, c)
	elseif n == 12 then
		return makecf(a, b, c, d, e, f, g, h, i, j, k, l)
	end
	error("CFrame.new: unsupported arguments", 2)
end

function CFrame.fromMatrix(pos, vx, vy)
	local vz = vx:Cross(vy)
	return makecf(pos._x, pos._y, pos._z, vx._x, vy._x, vz._x, vx._y, vy._y, vz._y, vx._z, vy._z, vz._z)
end

function CFrame.Angles(rx, ry, rz)
	return CFrame.fromEulerAnglesXYZ(rx, ry, rz)
end

function CFrame.fromEulerAnglesXYZ(rx, ry, rz)
	local cx, sx = cos(rx), sin(rx)
	local cy, sy = cos(ry), sin(ry)
	local cz, sz = cos(rz), sin(rz)
	local rx3 = { 1, 0, 0, 0, cx, -sx, 0, sx, cx }
	local ry3 = { cy, 0, sy, 0, 1, 0, -sy, 0, cy }
	local rz3 = { cz, -sz, 0, sz, cz, 0, 0, 0, 1 }
	return frompos(0, 0, 0, mul3(mul3(rx3, ry3), rz3))
end

function CFrame.fromOrientation(rx, ry, rz)
	return CFrame.fromEulerAnglesXYZ(rx, ry, rz)
end

function CFrame.lookAt(pos, target, up)
	up = up or Vector3.new(0, 1, 0)
	local look = (target - pos).Unit
	if look.Magnitude == 0 then look = Vector3.new(0, 0, -1) end
	local right = look:Cross(up).Unit
	if right.Magnitude == 0 then right = Vector3.new(1, 0, 0) end
	local realUp = right:Cross(look)
	return makecf(
		pos._x, pos._y, pos._z,
		right._x, realUp._x, -look._x,
		right._y, realUp._y, -look._y,
		right._z, realUp._z, -look._z
	)
end

CFrame.identity = frompos(0, 0, 0)

local function transformPoint(cf, p)
	return Vector3.new(
		cf._m11 * p._x + cf._m12 * p._y + cf._m13 * p._z + cf._x,
		cf._m21 * p._x + cf._m22 * p._y + cf._m23 * p._z + cf._y,
		cf._m31 * p._x + cf._m32 * p._y + cf._m33 * p._z + cf._z
	)
end

local function transformVector(cf, v)
	return Vector3.new(
		cf._m11 * v._x + cf._m12 * v._y + cf._m13 * v._z,
		cf._m21 * v._x + cf._m22 * v._y + cf._m23 * v._z,
		cf._m31 * v._x + cf._m32 * v._y + cf._m33 * v._z
	)
end

local function compose(a, b)
	local m = mul3(mat(a), mat(b))
	local t = transformPoint(a, Vector3.new(b._x, b._y, b._z))
	return frompos(t._x, t._y, t._z, m)
end

cfmeta.__index = function(self, k)
	if k == "Position" or k == "p" then return Vector3.new(self._x, self._y, self._z) end
	if k == "X" or k == "x" then return self._x end
	if k == "Y" or k == "y" then return self._y end
	if k == "Z" or k == "z" then return self._z end
	if k == "LookVector" then return Vector3.new(-self._m13, -self._m23, -self._m33) end
	if k == "RightVector" then return Vector3.new(self._m11, self._m21, self._m31) end
	if k == "UpVector" then return Vector3.new(self._m12, self._m22, self._m32) end
	if k == "XVector" then return Vector3.new(self._m11, self._m21, self._m31) end
	if k == "YVector" then return Vector3.new(self._m12, self._m22, self._m32) end
	if k == "ZVector" then return Vector3.new(self._m13, self._m23, self._m33) end
	return cfmethods[k]
end

cfmeta.__mul = function(a, b)
	if iscf(b) then return compose(a, b) end
	if isv3(b) then return transformPoint(a, b) end
	error("CFrame * " .. typeof(b) .. " is not supported", 2)
end
cfmeta.__add = function(a, v) return makecf(a._x + v._x, a._y + v._y, a._z + v._z, a._m11, a._m12, a._m13, a._m21, a._m22, a._m23, a._m31, a._m32, a._m33) end
cfmeta.__sub = function(a, v) return makecf(a._x - v._x, a._y - v._y, a._z - v._z, a._m11, a._m12, a._m13, a._m21, a._m22, a._m23, a._m31, a._m32, a._m33) end
cfmeta.__eq = function(a, b)
	return a._x == b._x and a._y == b._y and a._z == b._z and a._m11 == b._m11 and a._m22 == b._m22 and a._m33 == b._m33
end
cfmeta.__tostring = function(a)
	return string.format("%g, %g, %g, %g, %g, %g, %g, %g, %g, %g, %g, %g",
		a._x, a._y, a._z, a._m11, a._m12, a._m13, a._m21, a._m22, a._m23, a._m31, a._m32, a._m33)
end

CFrame.__index = CFrame
cfmethods.Inverse = function(cf)
	local m = mat(cf)
	return makecf(
		-(m[1] * cf._x + m[4] * cf._y + m[7] * cf._z),
		-(m[2] * cf._x + m[5] * cf._y + m[8] * cf._z),
		-(m[3] * cf._x + m[6] * cf._y + m[9] * cf._z),
		m[1], m[4], m[7], m[2], m[5], m[8], m[3], m[6], m[9]
	)
end
cfmethods.Lerp = function(a, b, t)
	local pa = Vector3.new(a._x, a._y, a._z)
	local pb = Vector3.new(b._x, b._y, b._z)
	local p = pa:Lerp(pb, t)
	return frompos(p._x, p._y, p._z, mat(a))
end
cfmethods.ToWorldSpace = function(cf, other) return compose(cf, other) end
cfmethods.ToObjectSpace = function(cf, other) return compose(cf:Inverse(), other) end
cfmethods.PointToWorldSpace = function(cf, v) return transformPoint(cf, v) end
cfmethods.PointToObjectSpace = function(cf, v) return transformPoint(cf:Inverse(), v) end
cfmethods.VectorToWorldSpace = function(cf, v) return transformVector(cf, v) end
cfmethods.VectorToObjectSpace = function(cf, v)
	local inv = cf:Inverse()
	return transformVector(inv, v)
end
cfmethods.GetComponents = function(cf)
	return cf._x, cf._y, cf._z, cf._m11, cf._m12, cf._m13, cf._m21, cf._m22, cf._m23, cf._m31, cf._m32, cf._m33
end
cfmethods.ToEulerAnglesXYZ = function(cf)
	local sy = -cf._m31
	if abs(sy) > 0.9999 then sy = sy > 0 and 1 or -1 end
	local ry = math.asin(sy)
	local rx = math.atan2(cf._m32, cf._m33)
	local rz = math.atan2(cf._m21, cf._m11)
	return rx, ry, rz
end
cfmethods.ToOrientation = function(cf)
	local rx, ry, rz = cfmethods.ToEulerAnglesXYZ(cf)
	return rx, ry, rz
end

--------------------------------------------------------------------------------
-- UDim / UDim2 / Rect / NumberRange
--------------------------------------------------------------------------------

local UDim = {}
local udimmeta = {}
local UDim2 = {}
local udim2meta = {}

function UDim.new(scale, offset)
	return setmetatable({ __datatype = "UDim", _s = scale or 0, _o = offset or 0 }, udimmeta)
end
udimmeta.__index = function(self, k)
	if k == "Scale" then return self._s end
	if k == "Offset" then return self._o end
	return UDim[k]
end
udimmeta.__add = function(a, b) return UDim.new(a._s + b._s, a._o + b._o) end
udimmeta.__sub = function(a, b) return UDim.new(a._s - b._s, a._o - b._o) end
udimmeta.__eq = function(a, b) return a._s == b._s and a._o == b._o end
udimmeta.__tostring = function(a) return string.format("{%g, %d}", a._s, a._o) end
UDim.__index = UDim

function UDim2.new(xs, xo, ys, yo)
	return setmetatable({ __datatype = "UDim2", _xs = xs or 0, _xo = xo or 0, _ys = ys or 0, _yo = yo or 0 }, udim2meta)
end
function UDim2.fromScale(x, y) return UDim2.new(x, 0, y, 0) end
function UDim2.fromOffset(x, y) return UDim2.new(0, x, 0, y) end
udim2meta.__index = function(self, k)
	if k == "X" then return UDim.new(self._xs, self._xo) end
	if k == "Y" then return UDim.new(self._ys, self._yo) end
	if k == "Width" then return UDim.new(self._xs, self._xo) end
	if k == "Height" then return UDim.new(self._ys, self._yo) end
	return UDim2[k]
end
udim2meta.__add = function(a, b)
	return UDim2.new(a._xs + b._xs, a._xo + b._xo, a._ys + b._ys, a._yo + b._yo)
end
udim2meta.__sub = function(a, b)
	return UDim2.new(a._xs - b._xs, a._xo - b._xo, a._ys - b._ys, a._yo - b._yo)
end
udim2meta.__eq = function(a, b)
	return a._xs == b._xs and a._xo == b._xo and a._ys == b._ys and a._yo == b._yo
end
udim2meta.__tostring = function(a)
	return string.format("{%g, %d}, {%g, %d}", a._xs, a._xo, a._ys, a._yo)
end
UDim2.__index = UDim2
function UDim2.Lerp(a, b, t)
	return UDim2.new(
		a._xs + (b._xs - a._xs) * t, a._xo + (b._xo - a._xo) * t,
		a._ys + (b._ys - a._ys) * t, a._yo + (b._yo - a._yo) * t)
end

local Rect = {}
local rectmeta = {}
function Rect.new(minx, miny, maxx, maxy)
	if minx ~= nil and miny == nil then
		return setmetatable({ __datatype = "Rect", _minx = minx.X.Offset, _miny = minx.Y.Offset, _maxx = maxx.X.Offset, _maxy = maxx.Y.Offset }, rectmeta)
	end
	return setmetatable({ __datatype = "Rect", _minx = minx or 0, _miny = miny or 0, _maxx = maxx or 0, _maxy = maxy or 0 }, rectmeta)
end
rectmeta.__index = function(self, k)
	if k == "Min" then return Vector2.new(self._minx, self._miny) end
	if k == "Max" then return Vector2.new(self._maxx, self._maxy) end
	if k == "Width" then return self._maxx - self._minx end
	if k == "Height" then return self._maxy - self._miny end
	return Rect[k]
end
Rect.__index = Rect

local NumberRange = {}
local nrmeta = {}
function NumberRange.new(a, b)
	return setmetatable({ __datatype = "NumberRange", _min = a or 0, _max = b == nil and a or b }, nrmeta)
end
nrmeta.__index = function(self, k)
	if k == "Min" then return self._min end
	if k == "Max" then return self._max end
	return NumberRange[k]
end
NumberRange.__index = NumberRange

local NumberSequenceKeypoint = {}
function NumberSequenceKeypoint.new(time, value, envelope)
	return { __datatype = "NumberSequenceKeypoint", Time = time, Value = value, Envelope = envelope or 0 }
end
NumberSequenceKeypoint.__index = NumberSequenceKeypoint

local NumberSequence = {}
function NumberSequence.new(a, b)
	local points = {}
	if type(a) == "table" then
		for _, p in ipairs(a) do points[#points + 1] = p end
	else
		points[1] = NumberSequenceKeypoint.new(0, a or 0)
		points[2] = NumberSequenceKeypoint.new(1, b == nil and (a or 0) or b)
	end
	return { __datatype = "NumberSequence", Keypoints = points }
end
NumberSequence.__index = NumberSequence

local ColorSequenceKeypoint = {}
function ColorSequenceKeypoint.new(time, color)
	return { __datatype = "ColorSequenceKeypoint", Time = time, Value = color }
end
ColorSequenceKeypoint.__index = ColorSequenceKeypoint

local ColorSequence = {}
function ColorSequence.new(a, b)
	local points = {}
	if type(a) == "table" and a.__datatype == nil then
		for _, p in ipairs(a) do points[#points + 1] = p end
	else
		points[1] = ColorSequenceKeypoint.new(0, a)
		points[2] = ColorSequenceKeypoint.new(1, b or a)
	end
	return { __datatype = "ColorSequence", Keypoints = points }
end
ColorSequence.__index = ColorSequence

local TweenInfo = {}
function TweenInfo.new(time, style, direction, repeatCount, reverses, delayTime)
	return {
		__datatype = "TweenInfo",
		Time = time or 1,
		EasingStyle = style or Enum.EasingStyle.Quad,
		EasingDirection = direction or Enum.EasingDirection.Out,
		RepeatCount = repeatCount or 0,
		Reverses = reverses or false,
		DelayTime = delayTime or 0,
	}
end
TweenInfo.__index = TweenInfo

--------------------------------------------------------------------------------
-- Enum
--------------------------------------------------------------------------------

local Enum = {}

local function makeEnum(name, names)
	local e = { __datatype = "Enum", Name = name, _items = {} }
	for index, itemName in ipairs(names) do
		local item = {
			__datatype = "EnumItem",
			Name = itemName,
			Value = index - 1,
			EnumType = e,
		}
		e._items[index] = item
		e[itemName] = item
	end
	function e:GetEnumItems()
		local copy = {}
		for i, v in ipairs(self._items) do copy[i] = v end
		return copy
	end
	return e
end

local function splitEnumList(s)
	local out = {}
	for item in string.gmatch(s, "[^,]+") do
		out[#out + 1] = (item:gsub("^%s+", ""):gsub("%s+$", ""))
	end
	return out
end

local ENUMS = {
	Material = "Plastic,Neon,SmoothPlastic,Wood,WoodPlanks,Marble,Slate,Concrete,Granite,Brick,Pebble,Cobblestone,Rock,Sandstone,Basalt,CrackedLava,Limestone,Pavement,CorrodedMetal,DiamondPlate,Foil,Metal,Grass,LeafyGrass,Sand,Fabric,Snow,Mud,Ground,Asphalt,Salt,Ice,Glacier,Glass,ForceField,Air,Water",
	PartType = "Ball,Block,Cylinder,Wedge,CornerWedge",
	HumanoidStateType = "FallingDown,Running,RunningNoPhysics,Climbing,StrafingNoPhysics,Ragdoll,Jumping,Landed,Flying,Freefall,Seated,PlatformStanding,Swimming,Dead,Physics",
	HumanoidRigType = "R6,R15",
	EasingStyle = "Linear,Sine,Back,Quad,Quart,Quint,Bounce,Elastic,Exponential,Circular,Cubic",
	EasingDirection = "In,Out,InOut",
	NormalId = "Right,Top,Back,Left,Bottom,Front",
	Axis = "X,Y,Z",
	CameraType = "Fixed,Attach,Watch,Track,Follow,Custom,Scriptable,Orbital",
	CameraMode = "Classic,Fixed,Orbital",
	Technology = "Legacy,Voxel,Compatibility,ShadowMap,Future",
	RenderPriority = "First,Input,Camera,Character,Last",
	RunContext = "Legacy,Server,Client",
	UserInputType = "MouseButton1,MouseButton2,MouseButton3,MouseMovement,Touch,Keyboard,Gamepad1,Gamepad2,Gamepad3,Gamepad4,Focus,None",
	UserInputState = "Begin,Change,End,Cancel,None",
	KeyCode = "Unknown,Backspace,Tab,Return,Escape,Space,LeftShift,RightShift,LeftControl,RightControl,LeftAlt,RightAlt,Up,Down,Left,Right,A,B,C,D,E,F,G,H,I,J,K,L,M,N,O,P,Q,R,S,T,U,V,W,X,Y,Z,Zero,One,Two,Three,Four,Five,Six,Seven,Eight,Nine",
	Font = "Legacy,Arial,ArialBold,SourceSans,SourceSansBold,SourceSansLight,SourceSansItalic,Code,Gotham,GothamMedium,GothamBold,GothamBlack",
	TextXAlignment = "Left,Center,Right",
	TextYAlignment = "Top,Center,Bottom",
	SizeConstraint = "RelativeXY,RelativeXX,RelativeYY",
	AutomaticSize = "None,X,Y,XY",
	FillDirection = "Horizontal,Vertical",
	HorizontalAlignment = "Left,Center,Right",
	VerticalAlignment = "Top,Center,Bottom",
	SortOrder = "LayoutOrder,Name,Custom",
	ScaleType = "Stretch,Fit,Slice,Tile,Crop",
	RollOffMode = "Inverse,InverseTapered,Linear,LinearSquare",
	SoundType = "NoSound,Boing,Bomb,Break,Click,Clock,Slingshot,Page,Snap,Pop,Snap,Sparkles",
	RaycastFilterType = "Exclude,Include",
	RaycastResult = "",
}

for name, items in pairs(ENUMS) do
	if items ~= "" then
		Enum[name] = makeEnum(name, splitEnumList(items))
	end
end

-- Deterministic pseudo-random (Luau's Random matches this interface closely enough
-- for game logic that just needs repeatable values).
local Random = {}
local rmeta = {}
function Random.new(seed)
	seed = seed or os.time()
	return setmetatable({ __datatype = "Random", _state = (seed % 2147483647) + 1 }, rmeta)
end
rmeta.__index = function(self, k)
	if k == "NextNumber" then
		return function(_self)
			_self._state = (_self._state * 16807) % 2147483647
			return _self._state / 2147483647
		end
	elseif k == "NextInteger" then
		return function(_self, min, max)
			_self._state = (_self._state * 16807) % 2147483647
			local t = _self._state / 2147483647
			return floor(min + t * (max - min + 1))
		end
	elseif k == "NextUnitVector" then
		return function(_self)
			_self._state = (_self._state * 16807) % 2147483647
			local a = _self._state / 2147483647 * 2 * math.pi
			_self._state = (_self._state * 16807) % 2147483647
			local z = _self._state / 2147483647 * 2 - 1
			local r = sqrt(1 - z * z)
			return Vector3.new(cos(a) * r, sin(a) * r, z)
		end
	elseif k == "NextNumberRange" then
		return function(_self, range)
			_self._state = (_self._state * 16807) % 2147483647
			local t = _self._state / 2147483647
			return range.Min + t * (range.Max - range.Min)
		end
	end
	return Random[k]
end
Random.__index = Random
function Random.new() return setmetatable({ __datatype = "Random", _state = os.time() % 2147483647 + 1 }, rmeta) end

--------------------------------------------------------------------------------
-- Globals that are pure Luau
--------------------------------------------------------------------------------

_G.Vector2 = Vector2
_G.Vector3 = Vector3
_G.Color3 = Color3
_G.CFrame = CFrame
_G.UDim = UDim
_G.UDim2 = UDim2
_G.Rect = Rect
_G.NumberRange = NumberRange
_G.NumberSequence = NumberSequence
_G.NumberSequenceKeypoint = NumberSequenceKeypoint
_G.ColorSequence = ColorSequence
_G.ColorSequenceKeypoint = ColorSequenceKeypoint
_G.TweenInfo = TweenInfo
_G.Enum = Enum
_G.Random = Random

-- TweenService is implemented in Rust; keep the enum-style constants available
-- without instantiating anything.

local prelude = {
	Vector2 = Vector2,
	Vector3 = Vector3,
	Color3 = Color3,
	CFrame = CFrame,
	Enum = Enum,
}
return prelude
