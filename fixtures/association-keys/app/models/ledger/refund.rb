module Ledger
  class Refund < ApplicationRecord
    has_many :entries, class_name: "Entry", as: "payable", foreign_type: :payer_kind, foreign_key: :payable_slug, primary_key: :slug
  end
end
