local th = require("maki.test_helpers")
local helpers = require("tests.helpers")
local case = th.case
local idx = helpers.idx
local idx_with_meta = helpers.idx_with_meta
local has = helpers.has
local lacks = helpers.lacks

case("vue_composition_api_setup", function()
  local src = [==[<template>
  <div class="user-card" id="profile">
    <UserAvatar :url="avatar" />
    <section class="bio-section">
      <p id="bio">{{ bio }}</p>
    </section>
    <button class="btn primary" @click="handleLogout">Logout</button>
  </div>
</template>

<script setup lang="ts">
import { ref, computed } from 'vue'
import UserAvatar from './UserAvatar.vue'

interface Props {
  name: string
  avatar?: string
}

const props = defineProps<Props>()
const emit = defineEmits(['logout', 'update'])

const isOnline = ref(true)
const displayName = computed(() => `${props.name} (online)`)

function handleLogout() {
  emit('logout')
}
</script>

<style scoped>
.user-card {
  padding: 16px;
}
</style>
]==]

  local out = idx(src, "vue")
  has(out, {
    "blocks:",
    "<template>",
    "<script setup lang=\"ts\">",
    "<style scoped>",
    "template:",
    "<div#profile.user-card>",
    "<UserAvatar>",
    "<section.bio-section>",
    "<p#bio>",
    "<button.btn.primary>",
    "imports:",
    "UserAvatar from './UserAvatar.vue'",
    "{ ref, computed } from 'vue'",
    "types:",
    "interface Props",
    "name: string",
    "consts:",
    "props = defineProps<Props>()",
    "emit = defineEmits(['logout', 'update'])",
    "isOnline = ref(true)",
    "fns:",
    "handleLogout()",
  })
end)

case("vue_options_api", function()
  local src = [==[<template>
  <div class="counter">
    <p>{{ count }}</p>
    <button @click="increment">+</button>
    <button @click="decrement">-</button>
  </div>
</template>

<script lang="ts">
import { defineComponent } from 'vue'

export default defineComponent({
  name: 'CounterComponent',
  props: {
    initial: {
      type: Number,
      default: 0
    }
  },
  data() {
    return {
      count: 0
    }
  },
  computed: {
    doubled() {
      return this.count * 2
    }
  },
  methods: {
    increment() {
      this.count++
    },
    decrement() {
      this.count--
    }
  }
})
</script>
]==]

  local out = idx(src, "vue")
  has(out, {
    "blocks:",
    "<template>",
    "<script lang=\"ts\">",
    "template:",
    "<div.counter>",
    "imports:",
    "{ defineComponent } from 'vue'",
    "classes:",
    "export default defineComponent (CounterComponent)",
    "fns:",
    "increment()",
    "decrement()",
    "doubled() [computed]",
  })
end)

case("vue_template_only", function()
  local src = [==[<template>
  <main class="page-container">
    <HeaderBar />
    <section id="content">
      <p>Plain text</p>
    </section>
  </main>
</template>

<style>
.page-container { margin: 0; }
</style>
]==]

  local out = idx(src, "vue")
  has(out, {
    "blocks:",
    "<template>",
    "<style>",
    "template:",
    "<main.page-container>",
    "<HeaderBar>",
    "<section#content>",
  })
  lacks(out, {
    "imports:",
    "consts:",
    "fns:",
    "types:",
  })
end)

case("vue_script_only", function()
  local src = [==[<script setup lang="ts">
import { ref } from 'vue'

export interface State {
  count: number
}

export const active = ref(true)

export function reset() {
  active.value = false
}
</script>
]==]

  local out = idx(src, "vue")
  has(out, {
    "blocks:",
    "<script setup lang=\"ts\">",
    "imports:",
    "{ ref } from 'vue'",
    "types:",
    "export interface State",
    "count: number",
    "consts:",
    "export active = ref(true)",
    "fns:",
    "export reset()",
  })
  lacks(out, {
    "template:",
  })
end)

case("vue_dual_script", function()
  local src = [==[<script lang="ts">
export interface GlobalConfig {
  apiUrl: string
}
</script>

<script setup lang="ts">
import { ref } from 'vue'

const enabled = ref(true)
function toggle() {
  enabled.value = !enabled.value
}
</script>

<template>
  <div class="toggle-box">
    <button @click="toggle">Toggle</button>
  </div>
</template>
]==]

  local out = idx(src, "vue")
  has(out, {
    "blocks:",
    "<script lang=\"ts\">",
    "<script setup lang=\"ts\">",
    "<template>",
    "template:",
    "<div.toggle-box>",
    "imports:",
    "{ ref } from 'vue'",
    "types:",
    "export interface GlobalConfig",
    "consts:",
    "enabled = ref(true)",
    "fns:",
    "toggle()",
  })
end)

case("vue_meta_ranges", function()
  local src = [==[<template>
  <div class="box">
    <ChildComp />
  </div>
</template>

<script setup lang="ts">
import { ref } from 'vue'

const val = ref(42)
function run() {}
</script>
]==]

  local text, meta = idx_with_meta(src, "vue")
  helpers.assert_ranged_meta(text, meta, { "<template>", "<script setup lang=\"ts\">", "val =", "run()" })
end)

case("vue_standalone_macros_and_slots", function()
  local src = [==[<template>
  <div class="wrapper">
    <slot name="header" :item="item" />
    <my-custom-component v-if="visible" />
    <RouterView />
    <slot />
  </div>
</template>

<script setup lang="ts">
defineProps<{
  id: string
  title?: string
}>()

defineEmits<{
  (e: 'change', val: string): void
}>()

defineOptions({
  name: 'StandaloneMacroComponent'
})
</script>
]==]

  local out = idx(src, "vue")
  has(out, {
    "blocks:",
    "<template>",
    "<script setup lang=\"ts\">",
    "template:",
    "<div.wrapper>",
    "<slot>",
    "<my-custom-component>",
    "<RouterView>",
    "types:",
    "defineProps<{ id: string title?: string }>()",
    "defineEmits<{ (e: 'change', val: string): void }>()",
    "consts:",
    "defineOptions({ name: 'StandaloneMacroComponent' })",
  })
end)

case("vue_syntax_error", function()
  local indexer = require("indexer")
  local src = "<template><div</template>"
  local out = idx(src, "vue")
  assert(out == indexer.PARSE_ERROR_NOTE .. "\n" or out:find("blocks:"), "expected error note or fallback")
end)
